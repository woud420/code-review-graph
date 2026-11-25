use super::{children, text};
use regex::Regex;
use std::sync::LazyLock;
use tree_sitter::Node;
static COMMENTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)/\*.*?\*/|//[^\r\n]*|\[\[.*?\]\]").unwrap());
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
static PUNCT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*(::|[<>,*&()\[\]])\s*").unwrap());

fn inner<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    if node.kind() == kind {
        return Some(node);
    }
    children(node)
        .into_iter()
        .filter(|c| !matches!(c.kind(), "parameter_list" | "template_argument_list"))
        .find_map(|v| inner(v, kind))
}

fn ident(node: Node<'_>) -> Option<Node<'_>> {
    if matches!(
        node.kind(),
        "identifier" | "field_identifier" | "operator_name" | "destructor_name"
    ) {
        return Some(node);
    }
    if let Some(v) = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("declarator"))
        && let Some(i) = ident(v)
    {
        return Some(i);
    }
    children(node)
        .into_iter()
        .filter(|c| !matches!(c.kind(), "parameter_list" | "template_argument_list"))
        .find_map(ident)
}

fn parameter(n: Node<'_>, s: &str) -> String {
    let end = n
        .children(&mut n.walk())
        .find(|c| c.kind() == "=")
        .map_or(n.end_byte(), |c| c.start_byte());
    let raw = if let Some(i) = n.child_by_field_name("declarator").and_then(ident) {
        format!(
            "{}{}",
            &s[n.start_byte()..i.start_byte()],
            &s[i.end_byte()..end]
        )
    } else {
        s[n.start_byte()..end].to_owned()
    };
    let raw = COMMENTS.replace_all(&raw, " ");
    let raw = SPACES.replace_all(raw.trim(), " ");
    PUNCT.replace_all(&raw, "$1").into_owned()
}

pub(super) struct Signature {
    pub name: String,
    pub scope: Option<String>,
    pub identity: String,
    pub params: Option<String>,
}

pub(super) fn extract(n: Node<'_>, s: &str, cpp: bool) -> Option<Signature> {
    let root = n.child_by_field_name("declarator")?;
    let cast = inner(root, "operator_cast");
    let fun = inner(root, "function_declarator")
        .or_else(|| cast.and_then(|c| inner(c, "abstract_function_declarator")))?;
    let decl = fun.child_by_field_name("declarator").or(cast)?;
    if decl.kind() == "parenthesized_declarator" && inner(decl, "function_declarator").is_none() {
        return None;
    }
    let name = if let Some(cast) = cast {
        format!("operator {}", text(cast.child_by_field_name("type")?, s))
    } else {
        text(ident(decl)?, s).to_owned()
    };
    if !cpp {
        return Some(Signature {
            name: name.clone(),
            scope: None,
            identity: name,
            params: None,
        });
    }
    let scope = inner(decl, "qualified_identifier").and_then(|v| {
        text(v, s)
            .rsplit_once("::")
            .map(|(ns, _)| ns.trim_start_matches("::").replace("::", "."))
    });
    let p = fun.child_by_field_name("parameters")?;
    let mut args: Vec<_> = children(p)
        .into_iter()
        .filter(|c| c.kind() != "comment")
        .map(|c| parameter(c, s))
        .filter(|p| !p.is_empty())
        .collect();
    if p.children(&mut p.walk()).any(|c| c.kind() == "...") {
        args.push("...".into());
    }
    if args == ["void"] {
        args.clear();
    }
    let mut identity = format!("{name}({})", args.join(","));
    for qualifier in children(fun)
        .into_iter()
        .filter(|c| matches!(c.kind(), "type_qualifier" | "ref_qualifier"))
    {
        identity.push(' ');
        identity.push_str(text(qualifier, s));
    }
    Some(Signature {
        name,
        scope,
        identity,
        params: Some(text(p, s).into()),
    })
}

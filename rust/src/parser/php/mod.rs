mod calls;
pub(crate) mod imports;
use super::syntax::{child, children, edge, field, qualified, record, text};
use crate::model::{ParsedFile, syntax::Definition};
use serde_json::json;
use std::collections::HashMap;
use tree_sitter::Node;

pub(super) fn extract(path: &std::path::Path, root: Node<'_>, s: &str) -> ParsedFile {
    let mut file = ParsedFile::default();
    let ns: Vec<_> = children(root)
        .into_iter()
        .filter(|n| n.kind() == "namespace_definition")
        .filter_map(|n| field(n, "name", s))
        .collect();
    if !ns.is_empty() {
        file.syntax.file = json!({"php_namespaces":ns});
    }
    declarations(
        root,
        s,
        &mut file,
        "",
        crate::model::test_file(&path.to_string_lossy(), None),
    );
    calls::extract(root, s, &mut file, "", "<module>", &mut HashMap::new());
    file
}
fn class(n: Node<'_>) -> bool {
    matches!(
        n.kind(),
        "class_declaration" | "interface_declaration" | "trait_declaration" | "enum_declaration"
    )
}
fn method(n: Node<'_>) -> bool {
    matches!(n.kind(), "method_declaration" | "function_definition")
}
fn doc(n: Node<'_>, s: &str) -> Option<String> {
    let c = n.prev_named_sibling()?;
    if c.kind() != "comment" || c.end_position().row + 1 < n.start_position().row {
        return None;
    }
    let d = text(c, s).strip_prefix("/**")?.strip_suffix("*/")?;
    Some(
        d.lines()
            .map(|l| l.trim().trim_start_matches('*').trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(400)
            .collect(),
    )
}
fn attributes(n: Node<'_>, s: &str, file: &ParsedFile) -> Vec<String> {
    let mut result = Vec::new();
    for a in n
        .child_by_field_name("attributes")
        .into_iter()
        .flat_map(children)
        .flat_map(children)
        .filter(|a| a.kind() == "attribute")
    {
        if let Some(name) = a.named_child(0) {
            let raw = text(name, s).trim_start_matches('\\');
            let fqn = file.php_imports.get(raw).map(String::as_str).unwrap_or(raw);
            result.push(
                if fqn.eq_ignore_ascii_case("PHPUnit\\Framework\\Attributes\\Test") {
                    "Test".into()
                } else {
                    raw.into()
                },
            );
        }
    }
    result
}
fn declarations(n: Node<'_>, s: &str, file: &mut ParsedFile, owner: &str, test_file: bool) {
    let mut nested = owner.to_owned();
    if n.kind() == "namespace_use_declaration" {
        imports::collect(n, s, file);
        return;
    }
    if (class(n) || method(n))
        && let Some(name) = field(n, "name", s)
    {
        let id = qualified(owner, name);
        let mut d = Definition {
            doc: doc(n, s),
            ..Default::default()
        };
        let kind = if class(n) {
            if matches!(n.kind(), "interface_declaration" | "trait_declaration") {
                d.extra = json!({"php_kind":n.kind().split('_').next().unwrap()});
            }
            nested = id.clone();
            for base in children(n)
                .into_iter()
                .filter(|n| matches!(n.kind(), "base_clause" | "class_interface_clause"))
                .flat_map(children)
            {
                if matches!(base.kind(), "name" | "qualified_name") {
                    edge(file, n, "INHERITS", &id, text(base, s));
                }
            }
            "Class"
        } else {
            d.params = field(n, "parameters", s).map(str::to_owned);
            d.decorators = attributes(n, s, file);
            if d.doc
                .as_deref()
                .is_some_and(|doc| doc.split_whitespace().any(|part| part == "@test"))
            {
                d.decorators.push("Test".into());
            }
            let phpunit_name = test_file && !owner.is_empty() && name.starts_with("test");
            if phpunit_name
                || name.starts_with("test_")
                || name.starts_with("Test")
                || name.ends_with("_test")
                || name.ends_with("_spec")
                || d.decorators.iter().any(|v| v == "Test")
            {
                "Test"
            } else {
                "Function"
            }
        };
        record(file, n, &id, kind);
        file.syntax.declarations.insert(id, d);
    }
    for c in children(n) {
        declarations(c, s, file, &nested, test_file);
    }
}

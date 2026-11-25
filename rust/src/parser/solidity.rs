use super::syntax::{
    call, child, children, edge, field, qualified, record, resolve_local_calls, text,
};
use crate::model::{ParsedFile, syntax::Definition};
use serde_json::json;
use tree_sitter::Node;

pub(super) fn extract(n: Node<'_>, s: &str) -> ParsedFile {
    let mut file = ParsedFile::default();
    walk(n, s, &mut file, "", "<module>");
    resolve_local_calls(&mut file);
    file
}

fn documentation(n: Node<'_>, s: &str) -> Option<String> {
    let mut comments = Vec::new();
    let mut previous = n.prev_named_sibling();
    let mut start = n.start_position().row;
    while let Some(p) = previous {
        if p.kind() != "comment" || p.end_position().row + 1 < start {
            break;
        }
        let value = text(p, s).trim();
        if let Some(doc) = value.strip_prefix("///") {
            comments.push(doc.trim().into());
        } else if let Some(doc) = value.strip_prefix("/**").and_then(|v| v.strip_suffix("*/")) {
            comments.push(
                doc.lines()
                    .map(|l| l.trim().trim_start_matches('*').trim())
                    .filter(|l| !l.is_empty())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        } else {
            break;
        }
        start = p.start_position().row;
        previous = p.prev_named_sibling();
    }
    comments.reverse();
    (!comments.is_empty()).then(|| comments.join(" "))
}

fn walk(n: Node<'_>, s: &str, file: &mut ParsedFile, class: &str, caller: &str) {
    let is_class = matches!(
        n.kind(),
        "contract_declaration"
            | "interface_declaration"
            | "library_declaration"
            | "struct_declaration"
            | "enum_declaration"
            | "error_declaration"
            | "user_defined_type_definition"
    );
    let is_function = matches!(
        n.kind(),
        "function_definition"
            | "constructor_definition"
            | "modifier_definition"
            | "event_definition"
            | "fallback_receive_definition"
    );
    let variable = matches!(
        n.kind(),
        "state_variable_declaration" | "constant_variable_declaration"
    );
    let mut class = class.to_owned();
    let mut caller = caller.to_owned();
    if is_class || is_function || variable {
        let name = match n.kind() {
            "constructor_definition" => Some("constructor"),
            "fallback_receive_definition" => n.child(0).map(|c| text(c, s)),
            _ => field(n, "name", s),
        };
        if let Some(name) = name {
            let identity = qualified(&class, name);
            let args: Vec<_> = children(n)
                .into_iter()
                .filter(|c| c.kind() == "parameter")
                .map(|c| text(c, s))
                .collect();
            let mut definition = Definition {
                params: (!args.is_empty()).then(|| format!("({})", args.join(", "))),
                returns: field(n, "return_type", s).map(str::to_owned),
                doc: documentation(n, s),
                extra: json!({}),
                ..Default::default()
            };
            if variable {
                definition.returns = field(n, "type", s).map(str::to_owned);
                definition.doc = None;
                definition.modifiers = field(n, "visibility", s).map(str::to_owned);
                definition.extra = if n.kind() == "state_variable_declaration" {
                    let mutability = n
                        .children(&mut n.walk())
                        .find(|c| matches!(c.kind(), "constant" | "immutable"))
                        .map(|c| c.kind());
                    json!({"solidity_kind":"state_variable","mutability":mutability})
                } else {
                    json!({"solidity_kind":"constant"})
                };
            }
            file.syntax
                .declarations
                .insert(identity.clone(), definition);
            record(
                file,
                n,
                &identity,
                if is_class { "Class" } else { "Function" },
            );
            if variable {
                return;
            }
            if is_class {
                class = identity;
                for base in children(n)
                    .into_iter()
                    .filter(|c| c.kind() == "inheritance_specifier")
                {
                    for id in child(base, "user_defined_type")
                        .into_iter()
                        .flat_map(children)
                        .filter(|c| c.kind() == "identifier")
                    {
                        edge(file, n, "INHERITS", &class, text(id, s));
                    }
                }
            } else {
                caller = identity;
            }
        }
    }
    match n.kind() {
        "import_directive" => {
            if let Some(p) = field(n, "source", s) {
                edge(file, n, "IMPORTS_FROM", "<module>", p.trim_matches('"'));
            }
        }
        "using_directive" => {
            if let Some(id) = child(n, "type_alias").and_then(|d| child(d, "identifier")) {
                edge(
                    file,
                    n,
                    "DEPENDS_ON",
                    if class.is_empty() { "<module>" } else { &class },
                    text(id, s),
                );
            }
        }
        "modifier_invocation" => {
            if let Some(id) = child(n, "identifier") {
                call(file, n, &caller, text(id, s));
            }
        }
        "emit_statement" => {
            if let Some(id) = n
                .child_by_field_name("name")
                .and_then(|d| child(d, "identifier"))
            {
                call(file, n, &caller, text(id, s));
            }
        }
        "call_expression" => {
            if let Some(f) = n
                .child_by_field_name("function")
                .and_then(|v| v.named_child(0))
            {
                let name = match f.kind() {
                    "identifier" => Some(text(f, s)),
                    "member_expression" => field(f, "property", s),
                    _ => None,
                };
                if let Some(name) = name {
                    call(file, n, &caller, name);
                }
            }
        }
        _ => {}
    }
    for c in children(n) {
        walk(c, s, file, &class, &caller);
    }
}

use super::super::syntax::{call, children, edge, field, qualified, text};
use super::{declare, function_kind};
use crate::model::{ParsedFile, syntax::Definition};
use tree_sitter::Node;
fn first<'t>(n: Node<'t>, kind: &str) -> Option<Node<'t>> {
    if n.kind() == kind {
        return Some(n);
    }
    children(n).into_iter().find_map(|c| first(c, kind))
}
pub(super) fn walk(n: Node<'_>, s: &str, file: &mut ParsedFile, scope: &str) {
    match n.kind() {
        "library_import" => {
            if let Some(uri) = first(n, "string_literal") {
                edge(
                    file,
                    n,
                    "IMPORTS_FROM",
                    "<module>",
                    text(uri, s).trim_matches(['\'', '"']),
                );
            }
            return;
        }
        "class_declaration"
        | "mixin_declaration"
        | "enum_declaration"
        | "extension_declaration" => {
            if let Some(name) = field(n, "name", s) {
                let id = qualified(scope, name);
                declare(file, n, &id, "Class", Default::default());
                for sup in children(n)
                    .into_iter()
                    .filter(|v| matches!(v.kind(), "superclass" | "interfaces" | "mixins"))
                {
                    let mut nodes = vec![sup];
                    while let Some(v) = nodes.pop() {
                        if v.kind() == "type" {
                            edge(file, n, "INHERITS", &id, text(v, s));
                        } else {
                            nodes.extend(children(v));
                        }
                    }
                }
                if let Some(body) = n.child_by_field_name("body") {
                    walk(body, s, file, &id);
                }
                return;
            }
        }
        "function_signature" => {
            if let Some(name) = field(n, "name", s) {
                declare(
                    file,
                    n,
                    &qualified(scope, name),
                    function_kind(name),
                    Definition {
                        params: field(n, "parameters", s).map(str::to_owned),
                        ..Default::default()
                    },
                );
                return;
            }
        }
        "call_expression" => {
            if let Some(fun) = n.child_by_field_name("function") {
                let name = if fun.kind() == "identifier" {
                    Some(text(fun, s))
                } else {
                    field(fun, "property", s)
                };
                if let Some(name) = name {
                    call(file, n, "<module>", name);
                }
            }
        }
        _ => {}
    }
    for c in children(n) {
        walk(c, s, file, scope);
    }
}

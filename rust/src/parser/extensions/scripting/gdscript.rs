use super::super::syntax::{call, child, children, edge, field, qualified, text};
use super::{declare, function_kind};
use crate::model::{ParsedFile, syntax::Definition};
use tree_sitter::Node;
pub(super) fn walk(n: Node<'_>, s: &str, file: &mut ParsedFile, scope: &str, caller: &str) {
    match n.kind() {
        "extends_statement" => {
            if let Some(ty) = child(n, "type") {
                edge(file, n, "IMPORTS_FROM", "<module>", text(ty, s));
                return;
            }
        }
        "class_name_statement" | "class_definition" => {
            if let Some(name) = field(n, "name", s) {
                let id = qualified(scope, name);
                declare(file, n, &id, "Class", Default::default());
                if let Some(body) = n.child_by_field_name("body") {
                    walk(body, s, file, &id, caller);
                }
                return;
            }
        }
        "function_definition" => {
            if let Some(name) = field(n, "name", s) {
                let id = qualified(scope, name);
                declare(
                    file,
                    n,
                    &id,
                    function_kind(name),
                    Definition {
                        params: field(n, "parameters", s).map(str::to_owned),
                        returns: field(n, "return_type", s).map(str::to_owned),
                        ..Default::default()
                    },
                );
                if let Some(body) = n.child_by_field_name("body") {
                    walk(body, s, file, scope, &id);
                }
                return;
            }
        }
        "call" | "attribute_call" => {
            if let Some(name) = child(n, "identifier") {
                call(file, n, caller, text(name, s));
            }
        }
        _ => {}
    }
    for c in children(n) {
        walk(c, s, file, scope, caller);
    }
}

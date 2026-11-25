use super::super::syntax::{call, child, children, edge, field, qualified, text};
use super::{declare, function_kind, identity::overload};
use crate::model::ParsedFile;
use tree_sitter::Node;
pub(super) fn walk(
    n: Node<'_>,
    s: &str,
    file: &mut ParsedFile,
    scope: &str,
    container: &str,
    caller: &str,
    test_file: bool,
) {
    match n.kind() {
        "preproc_include" => {
            edge(file, n, "IMPORTS_FROM", "<module>", text(n, s).trim());
            return;
        }
        "class_implementation" | "class_interface" | "protocol_declaration" => {
            if let Some(name) = child(n, "identifier").map(|v| text(v, s)) {
                let id = overload(file, scope, name, "Class", "objc", None, test_file);
                declare(file, n, &id, "Class", Default::default());
                for c in children(n) {
                    walk(c, s, file, &qualified(scope, name), &id, caller, test_file);
                }
                return;
            }
        }
        "method_definition" | "function_definition" => {
            let name = if n.kind() == "method_definition" {
                child(n, "identifier")
            } else {
                n.child_by_field_name("declarator")
                    .and_then(|d| d.child_by_field_name("declarator"))
            };
            if let Some(name) = name.map(|v| text(v, s)) {
                let id = qualified(scope, name);
                declare(file, n, &id, function_kind(name), Default::default());
                if container != scope {
                    file.syntax.containers.insert(id.clone(), container.into());
                }
                for c in children(n) {
                    walk(c, s, file, scope, container, &id, test_file);
                }
                return;
            }
        }
        "message_expression" => {
            if let Some(method) = field(n, "method", s) {
                call(file, n, caller, method);
            }
        }
        "call_expression" => {
            if let Some(name) = field(n, "function", s) {
                call(file, n, caller, name);
            }
        }
        _ => {}
    }
    for c in children(n) {
        walk(c, s, file, scope, container, caller, test_file);
    }
}

use super::super::syntax::{call, child, children, edge, field, qualified, text};
use super::{declare, function_kind, name_child};
use crate::model::{ParsedFile, syntax::Definition};
use tree_sitter::Node;

pub(super) fn walk(n: Node<'_>, s: &str, file: &mut ParsedFile, scope: &str, caller: &str) {
    match n.kind() {
        "function_declaration" => {
            if let Some(name) = field(n, "name", s) {
                let id = qualified(scope, name);
                declare(file, n, &id, function_kind(name), Default::default());
                if let Some(body) = n.child_by_field_name("body") {
                    walk(body, s, file, scope, &id);
                }
                return;
            }
        }
        "test_declaration" => {
            let label = child(n, "string")
                .map(|v| text(v, s).trim_matches('"'))
                .filter(|v| !v.is_empty());
            let base = label.map_or_else(|| "test".to_string(), |v| format!("test:{v}"));
            let id = qualified(scope, &format!("{base}@L{}", n.start_position().row + 1));
            declare(file, n, &id, "Test", Default::default());
            if let Some(body) = child(n, "block") {
                walk(body, s, file, scope, &id);
            }
            return;
        }
        "variable_declaration" => {
            if let Some(container) = name_child(n, |kind| {
                matches!(
                    kind,
                    "struct_declaration"
                        | "union_declaration"
                        | "enum_declaration"
                        | "opaque_declaration"
                )
            }) && let Some(name) = child(n, "identifier")
            {
                let id = qualified(scope, text(name, s));
                let kind = container.kind().strip_suffix("_declaration").unwrap();
                declare(
                    file,
                    n,
                    &id,
                    "Class",
                    Definition {
                        extra: serde_json::json!({"zig_kind":kind}),
                        ..Default::default()
                    },
                );
                for c in children(container) {
                    walk(c, s, file, &id, caller);
                }
                return;
            }
            if caller == "<module>"
                && let Some(builtin) = child(n, "builtin_function")
                && child(builtin, "builtin_identifier").is_some_and(|v| text(v, s) == "@import")
                && let Some(target) = child(builtin, "arguments").and_then(|a| child(a, "string"))
            {
                edge(
                    file,
                    n,
                    "IMPORTS_FROM",
                    "<module>",
                    text(target, s).trim_matches('"'),
                );
                return;
            }
        }
        "call_expression" => {
            if let Some(fun) = n.child_by_field_name("function") {
                let name = if fun.kind() == "identifier" {
                    Some(text(fun, s))
                } else {
                    field(fun, "member", s)
                };
                if let Some(name) = name {
                    call(file, n, caller, name);
                }
            }
        }
        "builtin_function" => {
            if let Some(name) = child(n, "builtin_identifier").map(|v| text(v, s))
                && name != "@import"
            {
                call(file, n, caller, name);
            }
        }
        _ => {}
    }
    for c in children(n) {
        walk(c, s, file, scope, caller);
    }
}

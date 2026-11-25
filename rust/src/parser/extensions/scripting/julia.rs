use super::super::syntax::{call, child, children, edge, field, qualified, text};
use super::{declare, function_kind};
use crate::model::{ParsedFile, syntax::Definition};
use serde_json::json;
use tree_sitter::Node;

fn descendant<'t>(n: Node<'t>, kind: &str) -> Option<Node<'t>> {
    if n.kind() == kind {
        return Some(n);
    }
    children(n).into_iter().find_map(|c| descendant(c, kind))
}
fn def(n: Node<'_>, callee: Node<'_>, s: &str, file: &mut ParsedFile, scope: &str) -> String {
    let name = text(callee, s);
    let id = qualified(scope, name);
    let mut extra = json!({});
    if let Some((prefix, _)) = name.rsplit_once('.') {
        extra["julia_module_qualifier"] = json!(prefix);
        edge(file, n, "REFERENCES", &id, prefix);
        file.syntax.edges.last_mut().unwrap().extra = json!({"julia_qualified_def":true});
        file.syntax.containers.insert(id.clone(), scope.into());
    }
    declare(
        file,
        n,
        &id,
        function_kind(name),
        Definition {
            extra,
            ..Default::default()
        },
    );
    id
}
pub(super) fn walk(n: Node<'_>, s: &str, file: &mut ParsedFile, scope: &str, caller: &str) {
    match n.kind() {
        "module_definition" => {
            if let Some(name) = field(n, "name", s) {
                let id = qualified(scope, name);
                declare(file, n, &id, "Class", Default::default());
                for c in children(n) {
                    walk(c, s, file, &id, &id);
                }
                return;
            }
        }
        "struct_definition" | "abstract_definition" | "primitive_definition" => {
            if let Some(head) = child(n, "type_head")
                && let Some(name) = descendant(head, "identifier")
            {
                let id = qualified(scope, text(name, s));
                declare(file, n, &id, "Class", Default::default());
                if let Some(expr) = child(head, "binary_expression")
                    && let Some(base) = expr.named_child(2)
                {
                    edge(file, n, "INHERITS", &id, text(base, s));
                }
                for c in children(n).into_iter().filter(|c| *c != head) {
                    walk(c, s, file, &id, &id);
                }
            }
            return;
        }
        "import_statement" | "using_statement" => {
            for spec in children(n) {
                if spec.kind() == "selected_import"
                    && let Some(base) = spec.named_child(0)
                {
                    for part in children(spec).into_iter().skip(1) {
                        edge(
                            file,
                            n,
                            "IMPORTS_FROM",
                            "<module>",
                            &qualified(text(base, s), text(part, s)),
                        );
                    }
                } else {
                    edge(file, n, "IMPORTS_FROM", "<module>", text(spec, s));
                }
            }
            return;
        }
        "export_statement" | "public_statement" => {
            for name in children(n) {
                edge(
                    file,
                    n,
                    "REFERENCES",
                    caller,
                    &qualified(scope, text(name, s)),
                );
                file.syntax.edges.last_mut().unwrap().extra = if n.kind() == "export_statement" {
                    json!({"julia_export":true})
                } else {
                    json!({"julia_public":true})
                };
            }
            return;
        }
        "function_definition" | "macro_definition" => {
            if let Some(sig) = child(n, "signature")
                && let Some(cal) = descendant(sig, "call_expression").and_then(|c| c.named_child(0))
            {
                let id = def(n, cal, s, file, scope);
                for c in children(n).into_iter().filter(|c| *c != sig) {
                    walk(c, s, file, &id, &id);
                }
            }
            return;
        }
        "assignment" => {
            if let Some(lhs) = n.named_child(0)
                && lhs.kind() == "call_expression"
                && let Some(cal) = lhs.named_child(0)
            {
                let id = def(n, cal, s, file, scope);
                for c in children(n).into_iter().skip(1) {
                    walk(c, s, file, &id, &id);
                }
                return;
            }
        }
        "macrocall_expression" => {
            if let Some(name) = child(n, "macro_identifier") {
                let raw = text(name, s);
                let args = child(n, "macro_argument_list")
                    .map(children)
                    .unwrap_or_default();
                if raw == "@enum"
                    && let Some(head) = args.first()
                {
                    let id = qualified(scope, text(*head, s));
                    declare(
                        file,
                        n,
                        &id,
                        "Class",
                        Definition {
                            extra: json!({"julia_kind":"enum"}),
                            ..Default::default()
                        },
                    );
                    for v in args
                        .into_iter()
                        .skip(1)
                        .filter(|c| c.kind() == "identifier")
                    {
                        declare(
                            file,
                            v,
                            &qualified(&id, text(v, s)),
                            "Function",
                            Definition {
                                extra: json!({"julia_kind":"enum_variant"}),
                                ..Default::default()
                            },
                        );
                    }
                    return;
                }
                if raw == "@testset" {
                    let label = args
                        .first()
                        .filter(|e| e.kind() == "string_literal")
                        .map(|e| text(*e, s).trim_matches('"'))
                        .unwrap_or("");
                    let id = qualified(
                        scope,
                        &format!("testset:{label}@L{}", n.start_position().row + 1),
                    );
                    declare(file, n, &id, "Test", Default::default());
                    for c in args {
                        walk(c, s, file, scope, &id);
                    }
                    return;
                }
                call(file, n, caller, raw);
                for c in args {
                    walk(c, s, file, scope, caller);
                }
                return;
            }
        }
        "call_expression" => {
            if let Some(head) = n.named_child(0) {
                let raw = text(head, s);
                let callee = raw.rsplit('.').next().unwrap_or(raw);
                if raw == "include"
                    && let Some(target) =
                        child(n, "argument_list").and_then(|args| child(args, "string_literal"))
                {
                    edge(
                        file,
                        n,
                        "IMPORTS_FROM",
                        "<module>",
                        text(target, s).trim_matches('"'),
                    );
                }
                if matches!(head.kind(), "identifier" | "field_expression") {
                    call(file, n, caller, callee);
                }
            }
        }
        _ => {}
    }
    for c in children(n) {
        walk(c, s, file, scope, caller);
    }
}

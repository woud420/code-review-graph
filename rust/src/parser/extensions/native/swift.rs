use super::super::syntax::{call, child, children, edge, field, qualified, text};
use super::identity::overload;
use super::{declare, function_kind};
use crate::model::{ParsedFile, syntax::Definition};
use tree_sitter::Node;

pub(super) fn walk(
    n: Node<'_>,
    s: &str,
    file: &mut ParsedFile,
    class: &str,
    container: &str,
    caller: &str,
    test_file: bool,
) {
    match n.kind() {
        "import_declaration" => {
            edge(file, n, "IMPORTS_FROM", "<module>", text(n, s));
            return;
        }
        "class_declaration" | "protocol_declaration" => {
            if let Some(name) = field(n, "name", s) {
                let id = overload(file, class, name, "Class", "swift", None, test_file);
                let token = n
                    .children(&mut n.walk())
                    .find(|v| {
                        matches!(
                            v.kind(),
                            "struct" | "enum" | "protocol" | "extension" | "actor" | "class"
                        )
                    })
                    .map(|v| v.kind())
                    .unwrap_or("class");
                declare(
                    file,
                    n,
                    &id,
                    "Class",
                    Definition {
                        extra: serde_json::json!({"swift_kind":token}),
                        ..Default::default()
                    },
                );
                for base in children(n)
                    .into_iter()
                    .filter(|c| c.kind() == "inheritance_specifier")
                {
                    if let Some(target) = field(base, "inherits_from", s) {
                        edge(file, n, "INHERITS", &id, target);
                    }
                }
                if let Some(body) = n.child_by_field_name("body") {
                    walk(
                        body,
                        s,
                        file,
                        &qualified(class, name),
                        &id,
                        caller,
                        test_file,
                    );
                }
                return;
            }
        }
        "init_declaration"
        | "deinit_declaration"
        | "subscript_declaration"
        | "function_declaration" => {
            let name = if n.kind() == "function_declaration" {
                field(n, "name", s)
            } else {
                n.kind().strip_suffix("_declaration")
            };
            if let Some(name) = name {
                let kind = function_kind(name);
                let id = overload(file, class, name, kind, "swift", None, test_file);
                declare(file, n, &id, kind, Default::default());
                if container != class {
                    file.syntax.containers.insert(id.clone(), container.into());
                }
                for c in children(n) {
                    walk(c, s, file, class, container, &id, test_file);
                }
                return;
            }
        }
        "call_expression" => {
            if let Some(fun) = n.named_child(0) {
                let target = if fun.kind() == "simple_identifier" {
                    Some(text(fun, s))
                } else {
                    child(fun, "navigation_suffix").and_then(|c| field(c, "suffix", s))
                };
                if let Some(raw) = target {
                    call(file, n, caller, raw);
                    if caller
                        .split_once(":S")
                        .is_some_and(|(base, _)| base.rsplit('.').next() == Some(raw))
                    {
                        file.syntax
                            .sites
                            .entry(n.start_position().row + 1)
                            .or_default()
                            .push(crate::model::syntax::Site {
                                caller: caller.into(),
                                raw: raw.into(),
                                target: Some(caller.into()),
                                extra: serde_json::json!({}),
                            });
                    }
                }
            }
        }
        _ => {}
    }
    for c in children(n) {
        walk(c, s, file, class, container, caller, test_file);
    }
}

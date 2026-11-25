use super::super::syntax::{call, child, children, edge, field, qualified, text};
use super::{declare, function_kind};
use crate::model::{ParsedFile, syntax::Definition};
use sha2::{Digest, Sha256};
use tree_sitter::Node;

fn identity(file: &ParsedFile, class: &str, name: &str, kind: &str) -> String {
    let id = qualified(class, name);
    if !file.syntax.declarations.contains_key(&id) {
        return id;
    }
    let parent = (!class.is_empty()).then_some(class);
    let payload = serde_json::to_string(&(
        kind,
        name,
        parent,
        "swift",
        None::<&str>,
        None::<&str>,
        None::<&str>,
        kind == "Test",
    ))
    .unwrap();
    let suffix = hex::encode(&Sha256::digest(payload)[..6]);
    let base = format!("{id}:S{suffix}");
    if !file.syntax.declarations.contains_key(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}:{n}"))
        .find(|v| !file.syntax.declarations.contains_key(v))
        .unwrap()
}

pub(super) fn walk(
    n: Node<'_>,
    s: &str,
    file: &mut ParsedFile,
    class: &str,
    container: &str,
    caller: &str,
) {
    match n.kind() {
        "import_declaration" => {
            edge(file, n, "IMPORTS_FROM", "<module>", text(n, s));
            return;
        }
        "class_declaration" | "protocol_declaration" => {
            if let Some(name) = field(n, "name", s) {
                let id = identity(file, class, name, "Class");
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
                    walk(body, s, file, &qualified(class, name), &id, caller);
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
                let id = identity(file, class, name, kind);
                declare(file, n, &id, kind, Default::default());
                if container != class {
                    file.syntax.containers.insert(id.clone(), container.into());
                }
                for c in children(n) {
                    walk(c, s, file, class, container, &id);
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
        walk(c, s, file, class, container, caller);
    }
}

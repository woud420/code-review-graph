use super::super::syntax::{call, child, children, edge, field, qualified, text};
use super::{declare, function_kind, identity::overload};
use crate::model::{
    ParsedFile,
    syntax::{Definition, Site},
};
use serde_json::json;
use tree_sitter::Node;

fn callee<'s>(n: Node<'_>, s: &'s str) -> Option<&'s str> {
    match n.kind() {
        "identifier" | "type_identifier" => Some(text(n, s)),
        "field_expression" => field(n, "field", s),
        "generic_function" => callee(n.child_by_field_name("function")?, s),
        _ => None,
    }
}
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
        "import_declaration" => {
            let path = n
                .children_by_field_name("path", &mut n.walk())
                .filter(|v| v.is_named())
                .map(|c| text(c, s))
                .collect::<Vec<_>>()
                .join(".");
            if let Some(selectors) = child(n, "namespace_selectors") {
                for item in children(selectors) {
                    let original = field(item, "name", s).unwrap_or(text(item, s));
                    edge(
                        file,
                        n,
                        "IMPORTS_FROM",
                        "<module>",
                        &qualified(&path, original),
                    );
                }
            } else if child(n, "namespace_wildcard").is_some() {
                edge(file, n, "IMPORTS_FROM", "<module>", &format!("{path}.*"));
            } else if !path.is_empty() {
                edge(file, n, "IMPORTS_FROM", "<module>", &path);
            }
            return;
        }
        "trait_definition" | "class_definition" | "object_definition" | "enum_definition" => {
            if let Some(name) = field(n, "name", s) {
                let id = overload(file, scope, name, "Class", "scala", None, test_file);
                declare(file, n, &id, "Class", Default::default());
                if let Some(extends) = child(n, "extends_clause") {
                    for base in extends
                        .children_by_field_name("type", &mut extends.walk())
                        .filter(|n| n.is_named())
                    {
                        edge(
                            file,
                            n,
                            "INHERITS",
                            &id,
                            field(base, "type", s).unwrap_or(text(base, s)),
                        );
                    }
                }
                if let Some(body) = n.child_by_field_name("body") {
                    walk(
                        body,
                        s,
                        file,
                        &qualified(scope, name),
                        &id,
                        caller,
                        test_file,
                    );
                }
                return;
            }
        }
        "function_declaration" | "function_definition" => {
            if let Some(name) = field(n, "name", s) {
                let params = field(n, "parameters", s);
                let kind = function_kind(name);
                let id = overload(file, scope, name, kind, "scala", params, test_file);
                declare(
                    file,
                    n,
                    &id,
                    kind,
                    Definition {
                        params: params.map(str::to_owned),
                        ..Default::default()
                    },
                );
                if container != scope {
                    file.syntax.containers.insert(id.clone(), container.into());
                }
                if let Some(body) = n.child_by_field_name("body") {
                    walk(body, s, file, scope, container, &id, test_file);
                }
                return;
            }
        }
        "call_expression" => {
            if let Some(name) = n.child_by_field_name("function").and_then(|c| callee(c, s)) {
                call(file, n, caller, name);
            }
        }
        "instance_expression" => {
            if let Some(name) = child(n, "type_identifier").map(|v| text(v, s)) {
                call(file, n, caller, name);
            }
        }
        _ => {}
    }
    for c in children(n) {
        walk(c, s, file, scope, container, caller, test_file);
    }
}
pub(super) fn collisions(file: &mut ParsedFile) {
    for c in &file.calls {
        let prefix = format!("{}:S", c.callee);
        if file.syntax.declarations.contains_key(&c.callee)
            && file
                .syntax
                .declarations
                .keys()
                .any(|id| id.starts_with(&prefix))
        {
            file.syntax.sites.entry(c.line).or_default().push(Site {
                raw: c.callee.clone(),
                caller: c.caller.clone(),
                target: None,
                extra: json!({}),
            });
        }
    }
}

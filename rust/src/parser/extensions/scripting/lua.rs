use super::syntax::{call, child, children, edge, field, qualified, record, text};
use crate::model::{
    ParsedFile,
    syntax::{Definition, Site},
};
use tree_sitter::Node;

pub(super) fn extract(root: Node<'_>, s: &str) -> ParsedFile {
    let mut file = ParsedFile::default();
    walk(root, s, &mut file, "", "<module>");
    for c in &file.calls {
        let target = file
            .syntax
            .declarations
            .contains_key(&c.callee)
            .then(|| c.callee.clone());
        file.syntax.sites.entry(c.line).or_default().push(Site {
            caller: c.caller.clone(),
            raw: c.callee.clone(),
            target,
            extra: serde_json::json!({}),
        });
    }
    file
}
fn require<'s>(n: Node<'_>, s: &'s str) -> Option<&'s str> {
    if n.kind() != "function_call" || field(n, "name", s) != Some("require") {
        return None;
    }
    n.child_by_field_name("arguments")
        .and_then(|a| child(a, "string"))
        .map(|v| {
            child(v, "string_content")
                .map_or_else(|| text(v, s).trim_matches(['\'', '"']), |c| text(c, s))
        })
}
fn function(
    file: &mut ParsedFile,
    n: Node<'_>,
    declaration: Node<'_>,
    name: &str,
    owner: &str,
    s: &str,
) -> String {
    let id = qualified(owner, name);
    let kind = if name.starts_with("test_")
        || name.starts_with("Test")
        || name.ends_with("_test")
        || name.ends_with("_spec")
    {
        "Test"
    } else {
        "Function"
    };
    record(file, n, &id, kind);
    file.syntax.declarations.insert(
        id.clone(),
        Definition {
            params: field(declaration, "parameters", s).map(str::to_owned),
            ..Default::default()
        },
    );
    id
}
fn walk(n: Node<'_>, s: &str, file: &mut ParsedFile, owner: &str, caller: &str) {
    if n.kind() == "variable_declaration"
        && let Some(assign) = child(n, "assignment_statement")
        && let Some(name) = child(assign, "variable_list").and_then(|v| child(v, "identifier"))
        && let Some(expressions) = child(assign, "expression_list")
    {
        for e in children(expressions) {
            if let Some(module) = require(e, s) {
                edge(file, n, "IMPORTS_FROM", "<module>", module);
                return;
            }
            if e.kind() == "function_definition" {
                let id = function(file, n, e, text(name, s), owner, s);
                for c in children(e) {
                    walk(c, s, file, owner, &id);
                }
                return;
            }
        }
    }
    if n.kind() == "function_declaration"
        && let Some(f) = n.child_by_field_name("name")
    {
        let ids: Vec<_> = children(f)
            .into_iter()
            .filter(|v| v.kind() == "identifier")
            .map(|v| text(v, s))
            .collect();
        let (owner, name) = if f.kind() == "identifier" {
            (owner, text(f, s))
        } else if let Some(&last) = ids.last() {
            (if ids.len() >= 2 { ids[0] } else { owner }, last)
        } else {
            return;
        };
        let id = function(file, n, n, name, owner, s);
        for c in children(n) {
            walk(c, s, file, owner, &id);
        }
        return;
    }
    if n.kind() == "type_definition"
        && let Some(name) = child(n, "identifier").map(|v| text(v, s))
    {
        let id = qualified(owner, name);
        record(file, n, &id, "Class");
        file.syntax.declarations.insert(id, Default::default());
    }
    if n.kind() == "function_call" {
        if caller == "<module>"
            && let Some(module) = require(n, s)
        {
            edge(file, n, "IMPORTS_FROM", "<module>", module);
            return;
        }
        if let Some(f) = n.child_by_field_name("name") {
            let method = match f.kind() {
                "identifier" => Some(text(f, s)),
                "method_index_expression" | "dot_index_expression" => children(f)
                    .into_iter()
                    .rev()
                    .find(|n| n.kind() == "identifier")
                    .map(|c| text(c, s)),
                _ => None,
            };
            if let Some(method) = method {
                call(file, n, caller, method);
            }
        }
    }
    for c in children(n) {
        walk(c, s, file, owner, caller);
    }
}

use super::super::syntax::{call, child, children, edge, field, qualified, text};
use super::{declare, function_kind};
use crate::model::{ParsedFile, syntax::Site};
use std::collections::BTreeMap;
use tree_sitter::Node;

pub(super) fn walk(
    n: Node<'_>,
    s: &str,
    file: &mut ParsedFile,
    scope: &str,
    caller: &str,
    types: &BTreeMap<String, String>,
) {
    match n.kind() {
        "import" => {
            if let Some(name) = child(n, "qualified_identifier") {
                edge(file, n, "IMPORTS_FROM", "<module>", text(name, s));
                return;
            }
        }
        "class_declaration" | "object_declaration" => {
            if let Some(name) = field(n, "name", s) {
                let id = qualified(scope, name);
                declare(file, n, &id, "Class", Default::default());
                edge(file, n, "INHERITS", &id, name);
                if let Some(specs) = child(n, "delegation_specifiers") {
                    for spec in children(specs) {
                        if let Some(base) = child(spec, "user_type").or_else(|| {
                            child(spec, "constructor_invocation")
                                .and_then(|c| child(c, "user_type"))
                        }) {
                            edge(file, n, "INHERITS", &id, text(base, s));
                        }
                    }
                }
                if let Some(body) = child(n, "class_body") {
                    walk(body, s, file, &id, caller, types);
                }
                return;
            }
        }
        "function_declaration" => {
            if let Some(name) = field(n, "name", s) {
                let id = qualified(scope, name);
                declare(file, n, &id, function_kind(name), Default::default());
                let mut local = types.clone();
                if let Some(params) = child(n, "function_value_parameters") {
                    for param in children(params) {
                        if let (Some(key), Some(value)) =
                            (child(param, "identifier"), child(param, "user_type"))
                        {
                            local.insert(text(key, s).into(), text(value, s).into());
                        }
                    }
                }
                if let Some(body) = child(n, "function_body") {
                    walk(body, s, file, scope, &id, &local);
                }
                return;
            }
        }
        "call_expression" => {
            if let Some(fun) = n.named_child(0) {
                if fun.kind() == "identifier" {
                    call(file, n, caller, text(fun, s));
                }
                if fun.kind() == "navigation_expression"
                    && let (Some(receiver), Some(name)) = (fun.named_child(0), fun.named_child(1))
                {
                    let name = text(name, s);
                    call(file, n, caller, name);
                    let receiver = text(receiver, s);
                    if let Some(ty) = types.get(receiver) {
                        file.syntax.sites.entry(n.start_position().row+1).or_default().push(Site{
                        caller:caller.into(), raw:name.into(),target:Some(qualified(ty,name)),
                        extra:serde_json::json!({"receiver":receiver,"receiver_type":ty,"receiver_resolution":"typed_receiver"})
                    });
                    }
                }
            }
        }
        _ => {}
    }
    for c in children(n) {
        walk(c, s, file, scope, caller, types);
    }
}

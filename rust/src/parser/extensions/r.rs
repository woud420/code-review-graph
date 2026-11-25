use super::super::syntax::{call, children, edge, field, qualified, text};
use super::{declare, function_kind};
use crate::model::{ParsedFile, syntax::Definition};
use tree_sitter::Node;

fn arguments(n: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    n.child_by_field_name("arguments")
        .into_iter()
        .flat_map(children)
        .filter(|v| v.kind() == "argument")
}

fn function(n: Node<'_>, source: &str, file: &mut ParsedFile, scope: &str, name: &str) {
    let id = qualified(scope, name);
    let kind = if scope.is_empty() {
        function_kind(name)
    } else {
        "Function"
    };
    declare(
        file,
        n,
        &id,
        kind,
        Definition {
            params: field(n, "parameters", source).map(str::to_owned),
            ..Default::default()
        },
    );
    for c in children(n) {
        walk(c, source, file, scope, &id);
    }
}

fn class(n: Node<'_>, s: &str, file: &mut ParsedFile, scope: &str, fallback: Option<&str>) -> bool {
    let name = arguments(n)
        .next()
        .and_then(|a| a.child_by_field_name("value"))
        .filter(|v| v.kind() == "string")
        .map(|v| text(v, s).trim_matches(['"', '\'']))
        .or(fallback);
    let Some(name) = name else {
        return false;
    };
    let owner = qualified(scope, name);
    declare(file, n, &owner, "Class", Default::default());
    if let Some(methods) = arguments(n)
        .find(|a| field(*a, "name", s) == Some("methods"))
        .and_then(|a| a.child_by_field_name("value"))
    {
        for a in arguments(methods) {
            if let (Some(method), Some(body)) =
                (field(a, "name", s), a.child_by_field_name("value"))
                && body.kind() == "function_definition"
            {
                function(body, s, file, &owner, method);
            }
        }
    }
    true
}

pub(super) fn walk(n: Node<'_>, s: &str, file: &mut ParsedFile, scope: &str, caller: &str) {
    if n.kind() == "binary_operator"
        && n.child(1).is_some_and(|op| matches!(op.kind(), "<-" | "="))
        && let (Some(lhs), Some(rhs)) = (n.child_by_field_name("lhs"), n.child_by_field_name("rhs"))
        && lhs.kind() == "identifier"
    {
        let name = text(lhs, s);
        if rhs.kind() == "function_definition" {
            function(rhs, s, file, scope, name);
            return;
        }
        if rhs.kind() == "call"
            && matches!(
                field(rhs, "function", s),
                Some("setClass" | "setRefClass" | "setGeneric")
            )
            && class(rhs, s, file, scope, Some(name))
        {
            return;
        }
    }
    if n.kind() == "call"
        && let Some(fun) = n.child_by_field_name("function")
    {
        let target = text(fun, s);
        if matches!(target, "library" | "require" | "source") {
            if let Some(value) = arguments(n)
                .next()
                .and_then(|a| a.child_by_field_name("value"))
                && matches!(value.kind(), "identifier" | "string")
            {
                edge(
                    file,
                    n,
                    "IMPORTS_FROM",
                    "<module>",
                    text(value, s).trim_matches(['"', '\'']),
                );
            }
            return;
        }
        if matches!(target, "setClass" | "setRefClass" | "setGeneric")
            && class(n, s, file, scope, None)
        {
            return;
        }
        if matches!(fun.kind(), "identifier" | "namespace_operator") && target != "return" {
            call(file, n, caller, target);
        }
    }
    for c in children(n) {
        walk(c, s, file, scope, caller);
    }
}

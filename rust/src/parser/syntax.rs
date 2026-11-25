use crate::model::{ParsedFile, Symbol};
use tree_sitter::Node;
pub(super) fn field<'s>(node: Node<'_>, key: &str, source: &'s str) -> Option<&'s str> {
    node.child_by_field_name(key)
        .map(|v| &source[v.byte_range()])
}

pub(super) fn direct_target(fun: Node<'_>, source: &str, class: &str) -> Option<String> {
    match fun.kind() {
        "identifier" => Some(source[fun.byte_range()].to_owned()),
        "generic_function" => direct_target(fun.child_by_field_name("function")?, source, class),
        "scoped_identifier" => {
            let scope = field(fun, "path", source)?;
            let name = field(fun, "name", source)?;
            Some(if scope == "Self" {
                format!("{class}.{name}")
            } else {
                format!("{scope}::{name}")
            })
        }
        "attribute" | "field_expression" => {
            let receiver = field(fun, "object", source).or_else(|| field(fun, "value", source))?;
            let name = field(fun, "attribute", source).or_else(|| field(fun, "field", source))?;
            if receiver == "self" && !class.is_empty() {
                Some(format!("{class}.{name}"))
            } else if fun.kind() == "field_expression"
                || receiver.split('.').all(|part| {
                    !part.is_empty() && part.chars().all(|ch| ch == '_' || ch.is_alphanumeric())
                })
            {
                Some(format!("{receiver}.{name}"))
            } else {
                None
            }
        }
        _ => None,
    }
}

pub(super) fn record(file: &mut ParsedFile, node: Node<'_>, name: &str, kind: &str) {
    file.symbols.push(Symbol {
        name: name.to_owned(),
        kind: kind.to_owned(),
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
    });
}

pub(super) fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    &source[node.byte_range()]
}

pub(super) fn children(node: Node<'_>) -> Vec<Node<'_>> {
    node.named_children(&mut node.walk()).collect()
}

pub(super) fn child<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    children(node).into_iter().find(|c| c.kind() == kind)
}

pub(super) fn qualified(owner: &str, name: &str) -> String {
    if owner.is_empty() {
        name.into()
    } else {
        format!("{owner}.{name}")
    }
}

pub(super) fn edge(file: &mut ParsedFile, node: Node<'_>, kind: &str, owner: &str, target: &str) {
    file.syntax.edges.push(crate::model::syntax::Edge {
        kind: kind.into(),
        owner: owner.into(),
        target: target.into(),
        line: node.start_position().row + 1,
        extra: serde_json::json!({}),
    });
}

pub(super) fn call(file: &mut ParsedFile, node: Node<'_>, caller: &str, callee: &str) {
    file.calls.push(crate::model::Call {
        caller: caller.into(),
        callee: callee.into(),
        line: node.start_position().row + 1,
    });
}

pub(super) fn resolve_local_calls(file: &mut ParsedFile) {
    use std::collections::HashMap;
    let mut by_name = HashMap::new();
    for symbol in &file.symbols {
        by_name
            .entry(symbol.name.rsplit('.').next().unwrap_or(&symbol.name))
            .or_insert(&symbol.name);
    }
    for call in &file.calls {
        let scoped = call
            .caller
            .rsplit_once('.')
            .map(|(parent, _)| qualified(parent, &call.callee));
        let target = scoped
            .filter(|s| file.syntax.declarations.contains_key(s))
            .or_else(|| by_name.get(call.callee.as_str()).map(|s| (*s).clone()));
        file.syntax
            .sites
            .entry(call.line)
            .or_default()
            .push(crate::model::syntax::Site {
                caller: call.caller.clone(),
                raw: call.callee.clone(),
                target,
                extra: serde_json::json!({}),
            });
    }
}

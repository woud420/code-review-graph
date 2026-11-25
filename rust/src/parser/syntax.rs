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

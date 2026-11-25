use std::path::Path;

use anyhow::{Context, Result, bail};
use tree_sitter::{Node, Parser};

use crate::model::{Call, ParsedFile, Symbol};

#[path = "python.rs"]
mod python;

pub fn parse_file(path: &Path, source: &str) -> Result<ParsedFile> {
    let grammar = match path.extension().and_then(|ext| ext.to_str()) {
        Some("py") => tree_sitter_python::LANGUAGE,
        Some("rs") => tree_sitter_rust::LANGUAGE,
        Some("js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts") => {
            return crate::typescript::parse_typescript(path, source);
        }
        _ => bail!("unsupported source file {}", path.display()),
    };
    let mut parser = Parser::new();
    parser.set_language(&grammar.into())?;
    let tree = parser.parse(source, None).context("parsing source")?;
    if path.extension().is_some_and(|e| e == "py") {
        return Ok(python::extract(tree.root_node(), source));
    }
    let mut file = ParsedFile::default();
    walk(tree.root_node(), source, &mut file, "", "<module>");
    Ok(file)
}

fn field<'s>(node: Node<'_>, key: &str, source: &'s str) -> Option<&'s str> {
    node.child_by_field_name(key)
        .map(|v| &source[v.byte_range()])
}

fn rust_test(node: Node<'_>, source: &str) -> bool {
    std::iter::successors(node.prev_named_sibling(), |n| n.prev_named_sibling())
        .take_while(|n| n.kind() == "attribute_item")
        .any(|attr| {
            let mut cursor = attr.walk();
            attr.named_children(&mut cursor).any(|n| {
                n.named_child(0)
                    .is_some_and(|key| source[key.byte_range()].rsplit("::").next() == Some("test"))
            })
        })
}

fn walk(node: Node<'_>, source: &str, file: &mut ParsedFile, class: &str, caller: &str) {
    let mut scope = class;
    let mut owner = caller;
    let qualified;
    match node.kind() {
        "struct_item" | "enum_item" | "trait_item" => {
            if let Some(name) = field(node, "name", source) {
                scope = name;
                record(file, node, name, "Class");
            }
        }
        "impl_item" => scope = field(node, "type", source).unwrap_or(""),
        "function_item" | "function_signature_item" => {
            if let Some(name) = field(node, "name", source) {
                qualified = if class.is_empty() {
                    name.to_owned()
                } else {
                    format!("{class}.{name}")
                };
                owner = &qualified;
                let is_test = rust_test(node, source);
                record(file, node, owner, if is_test { "Test" } else { "Function" });
            }
        }
        "macro_invocation" => {
            if let Some(name) = field(node, "macro", source) {
                file.calls.push(Call {
                    caller: caller.to_owned(),
                    callee: name.to_owned(),
                    line: node.start_position().row + 1,
                });
            }
        }
        "call_expression" => {
            if let Some(fun) = node.child_by_field_name("function")
                && let Some(target) = direct_target(fun, source, class)
            {
                file.calls.push(Call {
                    caller: caller.to_owned(),
                    callee: target,
                    line: node.start_position().row + 1,
                });
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(child, source, file, scope, owner);
    }
}

fn direct_target(fun: Node<'_>, source: &str, class: &str) -> Option<String> {
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

fn record(file: &mut ParsedFile, node: Node<'_>, name: &str, kind: &str) {
    file.symbols.push(Symbol {
        name: name.to_owned(),
        kind: kind.to_owned(),
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_and_python_keep_self_calls_scoped_to_the_type() {
        for (path, source) in [
            (
                "lib.rs",
                "struct R; impl R { fn b(&self) {} fn a(&self) { self.b(); } }",
            ),
            (
                "lib.py",
                "class R:\n    def b(self): pass\n    def a(self): self.b()",
            ),
        ] {
            let facts = parse_file(Path::new(path), source).unwrap();
            assert!(
                facts
                    .calls
                    .iter()
                    .any(|c| c.caller == "R.a" && c.callee == "R.b")
            );
        }
    }
}

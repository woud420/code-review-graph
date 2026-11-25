use std::path::Path;

use anyhow::{Context, Result, bail};
use tree_sitter::{Node, Parser};

use crate::model::{Call, Export, Import, ParsedFile, Symbol};

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
    let mut file = ParsedFile::default();
    walk(tree.root_node(), source, &mut file, "", "<module>");
    Ok(file)
}

fn field<'s>(node: Node<'_>, key: &str, source: &'s str) -> Option<&'s str> {
    node.child_by_field_name(key)
        .map(|v| &source[v.byte_range()])
}

fn export_python_name(file: &mut ParsedFile, name: &str) {
    file.exports.push(Export {
        name: name.into(),
        local: name.into(),
        module: None,
    });
}

fn python_imports(node: Node<'_>, source: &str, file: &mut ParsedFile) {
    let from = field(node, "module_name", source);
    let mut cursor = node.walk();
    for entry in node.children_by_field_name("name", &mut cursor) {
        let name = field(entry, "name", source).unwrap_or(&source[entry.byte_range()]);
        let local = field(entry, "alias", source).unwrap_or(name);
        file.imports.push(Import {
            local: local.into(),
            imported: from.map_or("*", |_| name).into(),
            module: from.unwrap_or(name).into(),
        });
        export_python_name(file, local);
    }
}

fn at_python_module(node: Node<'_>) -> bool {
    let mut parent = node.parent();
    if parent.is_some_and(|p| p.kind() == "decorated_definition") {
        parent = parent.and_then(|p| p.parent());
    }
    parent.is_some_and(|p| p.kind() == "module")
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
        "import_statement" | "import_from_statement" if at_python_module(node) => {
            python_imports(node, source, file);
        }
        "class_definition" | "struct_item" | "enum_item" | "trait_item" => {
            if let Some(name) = field(node, "name", source) {
                if node.kind() == "class_definition" && at_python_module(node) {
                    export_python_name(file, name);
                }
                scope = name;
                record(file, node, name, "Class");
            }
        }
        "impl_item" => scope = field(node, "type", source).unwrap_or(""),
        "function_definition" | "function_item" | "function_signature_item" => {
            if let Some(name) = field(node, "name", source) {
                qualified = if class.is_empty() {
                    name.to_owned()
                } else {
                    format!("{class}.{name}")
                };
                owner = &qualified;
                let is_test = if node.kind() == "function_definition" {
                    if at_python_module(node) {
                        export_python_name(file, name);
                    }
                    name.starts_with("test_")
                } else {
                    rust_test(node, source)
                };
                record(file, node, owner, if is_test { "Test" } else { "Function" });
            }
        }
        "call" | "call_expression" => {
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
        "scoped_identifier" => {
            let scope = field(fun, "path", source)?;
            let name = field(fun, "name", source)?;
            Some(format!(
                "{}.{name}",
                if scope == "Self" { class } else { scope }
            ))
        }
        "attribute" | "field_expression" => {
            let receiver = field(fun, "object", source).or_else(|| field(fun, "value", source))?;
            let name = field(fun, "attribute", source).or_else(|| field(fun, "field", source))?;
            if receiver == "self" && !class.is_empty() {
                Some(format!("{class}.{name}"))
            } else if fun.kind() == "attribute"
                && receiver.split('.').all(|part| {
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

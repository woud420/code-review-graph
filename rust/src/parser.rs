pub(crate) mod bash;
pub(crate) mod cfamily;
mod config;
mod csharp;
pub(crate) mod extensions;
pub(crate) mod go;
use config::{hcl, json};
mod java;
mod lua;
pub(crate) mod php;
pub(crate) mod ruby;
mod solidity;
pub(crate) mod sql;
mod syntax;
mod typescript;
pub(crate) use config::yaml;
use std::path::Path;
use syntax::{direct_target, field, record};

use anyhow::{Context, Result, bail};
use tree_sitter::{Node, Parser};

use crate::model::{Call, ParsedFile};

mod python;

pub fn parse_file(path: &Path, source: &str) -> Result<ParsedFile> {
    if let Some(language) = path
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(extensions::language)
    {
        return extensions::parse(language, path, source);
    }
    let grammar = match path.extension().and_then(|ext| ext.to_str()) {
        Some("rb") => return ruby::parse(path, source),
        Some("tf" | "hcl") => return hcl::parse(source),
        Some("properties") if yaml::spring::supports(path) => return Ok(yaml::parse(path, source)),
        Some("yaml" | "yml") => return Ok(yaml::parse(path, source)),
        Some(ext) if sql::supports(ext) => return sql::parse(path, source, None),
        Some(ext) if cfamily::supports(ext) => return cfamily::parse(path, source),
        Some("json") => return crate::parser::json::parse(path, source),
        Some("php") => tree_sitter_php::LANGUAGE_PHP,
        Some("lua") => tree_sitter_lua::LANGUAGE,
        Some("luau") => tree_sitter_luau::LANGUAGE,
        Some("sol") => tree_sitter_solidity::LANGUAGE,
        Some("cs") => tree_sitter_c_sharp::LANGUAGE,
        Some("java") => tree_sitter_java::LANGUAGE,
        Some("go") => tree_sitter_go::LANGUAGE,
        Some("py") => tree_sitter_python::LANGUAGE,
        Some("rs") => tree_sitter_rust::LANGUAGE,
        Some("js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts") => {
            return crate::parser::typescript::parse_typescript(path, source);
        }
        _ if crate::parser::bash::supports(path, source.as_bytes()) => tree_sitter_bash::LANGUAGE,
        _ => bail!("unsupported source file {}", path.display()),
    };
    let mut parser = Parser::new();
    parser.set_language(&grammar.into())?;
    let tree = parser.parse(source, None).context("parsing source")?;
    if matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("lua" | "luau")
    ) {
        return Ok(lua::extract(tree.root_node(), source));
    }
    if path.extension().is_some_and(|ext| ext == "php") {
        return Ok(php::extract(path, tree.root_node(), source));
    }
    if crate::parser::bash::supports(path, source.as_bytes()) {
        return Ok(crate::parser::bash::extract(tree.root_node(), source));
    }
    if path.extension().is_some_and(|e| e == "sol") {
        return Ok(solidity::extract(tree.root_node(), source));
    }
    if path.extension().is_some_and(|e| e == "cs") {
        return Ok(csharp::extract(tree.root_node(), source));
    }
    if path.extension().is_some_and(|e| e == "java") {
        return Ok(java::extract(tree.root_node(), source));
    }
    if path.extension().is_some_and(|e| e == "go") {
        return Ok(crate::parser::go::extract(path, tree.root_node(), source));
    }
    if path.extension().is_some_and(|e| e == "py") {
        return Ok(python::extract(tree.root_node(), source));
    }
    let mut file = ParsedFile::default();
    walk(tree.root_node(), source, &mut file, "", "<module>");
    Ok(file)
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

fn in_dead_rust_branch(node: Node<'_>, src: &str) -> bool {
    let mut next = node.parent();
    while let Some(parent) = next {
        if parent.kind() == "impl_item" {
            return false;
        }
        if parent.kind() == "if_expression"
            && let (Some(mut condition), Some(body)) = (
                parent.child_by_field_name("condition"),
                parent.child_by_field_name("consequence"),
            )
        {
            while condition.kind() == "parenthesized_expression"
                && let Some(inner) = condition.named_child(0)
            {
                condition = inner;
            }
            if condition.kind() == "boolean_literal"
                && &src[condition.byte_range()] == "false"
                && node.start_byte() >= body.start_byte()
                && node.end_byte() <= body.end_byte()
            {
                return true;
            }
        }
        next = parent.parent();
    }
    false
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
        "macro_invocation" | "call_expression" if in_dead_rust_branch(node, source) => return,
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

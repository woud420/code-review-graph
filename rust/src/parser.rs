pub(crate) mod bash;
pub(crate) mod cfamily;
mod config;
mod csharp;
pub(crate) mod extensions;
pub(crate) mod go;
use config::{hcl, json};
mod java;
#[path = "parser/extensions/scripting/lua.rs"]
mod lua;
pub(crate) mod php;
pub(crate) mod ruby;
pub(crate) mod rust;
mod solidity;
pub(crate) mod sql;
mod syntax;
mod typescript;
pub(crate) use config::yaml;
use std::path::Path;

use anyhow::{Context, Result, bail};
use tree_sitter::Parser;

use crate::model::ParsedFile;

mod python;

pub fn parse_file(path: &Path, source: &str) -> Result<ParsedFile> {
    if let Some(language) = path
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(extensions::language)
    {
        return extensions::parse(language, path, source);
    }
    if extensions::notebook::export(path, source) {
        return extensions::parse("notebook", path, source);
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
        return Ok(java::extract(
            tree.root_node(),
            source,
            crate::model::test_file(&path.to_string_lossy(), None),
        ));
    }
    if path.extension().is_some_and(|e| e == "go") {
        return Ok(crate::parser::go::extract(path, tree.root_node(), source));
    }
    if path.extension().is_some_and(|e| e == "py") {
        return Ok(python::extract(tree.root_node(), source));
    }
    Ok(rust::extract(tree.root_node(), source, path))
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

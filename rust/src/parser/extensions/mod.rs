mod kotlin;
mod nix;
mod r;
mod rescript;
mod swift;
mod xslt;
mod zig;

use super::syntax::{children, record};
use crate::model::{ParsedFile, syntax::Definition};
use anyhow::{Context, Result};
use std::path::Path;
use tree_sitter::{Node, Parser};

pub(crate) fn language(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "R" | "r" => "r",
        "xsl" | "xslt" => "xslt",
        "res" | "resi" => "rescript",
        "nix" => "nix",
        "zig" => "zig",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        _ => return None,
    })
}

pub(super) fn parse(language: &str, path: &Path, source: &str) -> Result<ParsedFile> {
    if language == "rescript" {
        return Ok(rescript::parse(path, source));
    }
    let grammar = match language {
        "xslt" => tree_sitter_xml::LANGUAGE_XML,
        "r" => tree_sitter_r::LANGUAGE,
        "kotlin" => tree_sitter_kotlin_ng::LANGUAGE,
        "swift" => tree_sitter_swift::LANGUAGE,
        "nix" => tree_sitter_nix::LANGUAGE,
        "zig" => tree_sitter_zig::LANGUAGE,
        _ => anyhow::bail!("unknown source language {language}"),
    };
    let mut parser = Parser::new();
    parser.set_language(&grammar.into())?;
    let tree = parser.parse(source, None).context("parsing source")?;
    let mut file = ParsedFile::default();
    let root = tree.root_node();
    match language {
        "kotlin" => kotlin::walk(root, source, &mut file, "", "<module>", &Default::default()),
        "swift" => swift::walk(root, source, &mut file, "", "", "<module>"),
        "xslt" => xslt::extract(root, source, &mut file),
        "r" => r::walk(root, source, &mut file, "", "<module>"),
        "nix" => nix::walk(
            root,
            source,
            &mut file,
            path.file_name().is_some_and(|v| v == "flake.nix"),
            false,
            false,
        ),
        "zig" => zig::walk(root, source, &mut file, "", "<module>"),
        _ => unreachable!(),
    }
    super::syntax::resolve_local_calls(&mut file);
    Ok(file)
}

pub(super) fn declare(
    file: &mut ParsedFile,
    node: Node<'_>,
    name: &str,
    kind: &str,
    data: Definition,
) {
    record(file, node, name, kind);
    file.syntax.declarations.insert(name.into(), data);
}

fn function_kind(name: &str) -> &'static str {
    if name.starts_with("test_")
        || name.starts_with("Test")
        || name.ends_with("_test")
        || name.ends_with("_spec")
    {
        "Test"
    } else {
        "Function"
    }
}

fn name_child<'t>(node: Node<'t>, pred: impl Fn(&str) -> bool) -> Option<Node<'t>> {
    children(node).into_iter().find(|n| pred(n.kind()))
}

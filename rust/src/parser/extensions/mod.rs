#[path = "scripting/dart.rs"]
mod dart;
#[path = "scripting/elixir.rs"]
mod elixir;
#[path = "scripting/gdscript.rs"]
mod gdscript;
mod identity;
#[path = "scripting/julia.rs"]
mod julia;
#[path = "jvm/kotlin.rs"]
mod kotlin;
#[path = "markup/liquid.rs"]
pub(crate) mod liquid;
mod nix;
pub(crate) mod notebook;
#[path = "native/objc.rs"]
mod objc;
#[path = "scripting/perl.rs"]
mod perl;
#[path = "scripting/r.rs"]
mod r;
mod rescript;
#[path = "jvm/scala.rs"]
mod scala;
#[path = "native/swift.rs"]
mod swift;
#[path = "native/verilog.rs"]
mod verilog;
#[path = "markup/web/mod.rs"]
pub(crate) mod web;
#[path = "markup/xslt/mod.rs"]
mod xslt;
#[path = "native/zig.rs"]
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
        "ipynb" => "notebook",
        "jl" => "julia",
        "sv" | "svh" | "v" | "vh" => "verilog",
        "ex" | "exs" => "elixir",
        "dart" => "dart",
        "scala" => "scala",
        "gd" => "gdscript",
        "m" => "objc",
        "pl" | "pm" | "t" => "perl",
        "liquid" => "liquid",
        "html" | "htm" => "html",
        "vue" => "vue",
        "nix" => "nix",
        "zig" => "zig",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        _ => return None,
    })
}

pub(super) fn parse(language: &str, path: &Path, source: &str) -> Result<ParsedFile> {
    if language == "liquid" {
        return Ok(liquid::parse(source));
    }
    if matches!(language, "html" | "vue") {
        return web::Page::read(language, source)?.extract();
    }
    if language == "notebook" {
        return notebook::parse(path, source);
    }
    if language == "rescript" {
        return Ok(rescript::parse(path, source));
    }
    let grammar = match language {
        "xslt" => tree_sitter_xml::LANGUAGE_XML,
        "r" => tree_sitter_r::LANGUAGE,
        "kotlin" => tree_sitter_kotlin_ng::LANGUAGE,
        "swift" => tree_sitter_swift::LANGUAGE,
        "julia" => tree_sitter_julia::LANGUAGE,
        "verilog" => tree_sitter_verilog::LANGUAGE,
        "elixir" => tree_sitter_elixir::LANGUAGE,
        "dart" => tree_sitter_dart::LANGUAGE,
        "scala" => tree_sitter_scala::LANGUAGE,
        "gdscript" => tree_sitter_gdscript::LANGUAGE,
        "objc" => tree_sitter_objc::LANGUAGE,
        "perl" => tree_sitter_perl::LANGUAGE,
        "nix" => tree_sitter_nix::LANGUAGE,
        "zig" => tree_sitter_zig::LANGUAGE,
        _ => anyhow::bail!("unknown source language {language}"),
    };
    let mut parser = Parser::new();
    parser.set_language(&grammar.into())?;
    let tree = parser.parse(source, None).context("parsing source")?;
    let mut file = ParsedFile::default();
    let root = tree.root_node();
    let test_file = crate::model::test_file(&path.to_string_lossy(), None);
    match language {
        "kotlin" => kotlin::walk(root, source, &mut file, "", "<module>", &Default::default()),
        "swift" => swift::walk(root, source, &mut file, "", "", "<module>", test_file),
        "xslt" => xslt::extract(root, source, &mut file),
        "julia" => julia::walk(root, source, &mut file, "", "<module>"),
        "verilog" => verilog::walk(root, source, &mut file, "", "<module>"),
        "elixir" => elixir::walk(root, source, &mut file, "", "<module>"),
        "dart" => dart::walk(root, source, &mut file, ""),
        "scala" => {
            scala::walk(root, source, &mut file, "", "", "<module>", test_file);
            scala::collisions(&mut file);
        }
        "gdscript" => gdscript::walk(root, source, &mut file, "", "<module>"),
        "objc" => objc::walk(root, source, &mut file, "", "", "<module>", test_file),
        "perl" => perl::walk(root, source, &mut file, "<module>", test_file),
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

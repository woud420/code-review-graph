#[path = "extra_edges.rs"]
mod extra_edges;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use tree_sitter::{Node, Parser};

#[derive(Default)]
pub(super) struct Declaration {
    pub params: Option<String>,
    pub returns: Option<String>,
    pub modifiers: Option<String>,
    pub doc: Option<String>,
    pub decorators: Vec<String>,
}

pub(super) struct Source {
    pub extra_edges: extra_edges::ExtraEdges,
    pub language: &'static str,
    pub lines: usize,
    pub declarations: BTreeMap<(usize, String), Declaration>,
    pub imports: BTreeSet<(usize, String)>,
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

fn field<'a>(node: Node<'_>, name: &str, source: &'a str) -> Option<&'a str> {
    node.child_by_field_name(name)
        .map(|value| text(value, source))
}

impl Source {
    pub fn claimed_call(
        &self,
        root: &Path,
        file: &str,
        callee: &str,
        facts: &crate::model::ParsedFile,
        paths: &BTreeSet<String>,
    ) -> Option<(String, String)> {
        if self.language != "python" {
            return None;
        }
        let binding = facts
            .imports
            .iter()
            .rev()
            .find(|b| b.local == callee && b.imported != "*")?;
        let origin = import_target(file, &binding.module, "python", paths)?;
        let claimed = format!("{}::{}", root.join(origin).display(), binding.imported);
        Some((binding.imported.clone(), claimed))
    }

    pub fn parse(path: &Path, source: &str) -> Result<Self> {
        let (language, grammar) = match path.extension().and_then(|ext| ext.to_str()) {
            Some("py") => ("python", tree_sitter_python::LANGUAGE),
            Some("rs") => ("rust", tree_sitter_rust::LANGUAGE),
            Some("tsx") => ("tsx", tree_sitter_typescript::LANGUAGE_TSX),
            Some("ts" | "cts" | "mts") => {
                ("typescript", tree_sitter_typescript::LANGUAGE_TYPESCRIPT)
            }
            Some("js" | "jsx" | "mjs" | "cjs") => ("javascript", tree_sitter_javascript::LANGUAGE),
            _ => bail!("unsupported source language for {}", path.display()),
        };
        let mut parser = Parser::new();
        parser.set_language(&grammar.into())?;
        let tree = parser
            .parse(source, None)
            .context("parsing declaration signatures")?;
        let mut result = Self {
            extra_edges: extra_edges::ExtraEdges::collect(tree.root_node(), source, language),
            language,
            lines: source.bytes().filter(|b| *b == b'\n').count() + 1,
            declarations: BTreeMap::new(),
            imports: BTreeSet::new(),
        };
        let mut pending = vec![tree.root_node()];
        while let Some(node) = pending.pop() {
            let line = node.start_position().row + 1;
            match node.kind() {
                "function_definition"
                | "function_item"
                | "function_signature_item"
                | "function_declaration"
                | "generator_function_declaration"
                | "method_definition"
                | "class_definition"
                | "arrow_function"
                | "function_expression" => {
                    let name = field(node, "name", source).or_else(|| {
                        let parent = node.parent()?;
                        (parent.kind() == "variable_declarator")
                            .then(|| field(parent, "name", source))
                            .flatten()
                    });
                    if let Some(name) = name {
                        let doc = (language == "python")
                            .then(|| super::docs::python(node, source))
                            .flatten();
                        let is_async = node.child(0).is_some_and(|n| n.kind() == "async");
                        result.declarations.insert(
                            (line, name.to_owned()),
                            Declaration {
                                params: field(node, "parameters", source).map(str::to_owned),
                                returns: if language == "rust" {
                                    None
                                } else {
                                    field(node, "return_type", source).map(str::to_owned)
                                },
                                modifiers: is_async.then(|| "async".into()),
                                doc,
                                decorators: Vec::new(),
                            },
                        );
                    }
                }
                "use_declaration" => {
                    if let Some(arg) = node.child_by_field_name("argument") {
                        rust_use_paths(arg, source, "", line, &mut result.imports);
                    }
                }
                "import_from_statement" => {
                    if let Some(module) = field(node, "module_name", source) {
                        result.imports.insert((line, module.to_owned()));
                    }
                }
                "import_statement" if language == "python" => {
                    let mut cursor = node.walk();
                    for item in node.children_by_field_name("name", &mut cursor) {
                        result.imports.insert((
                            line,
                            field(item, "name", source)
                                .unwrap_or(text(item, source))
                                .to_owned(),
                        ));
                    }
                }
                "import_statement" | "export_statement" => {
                    if let Some(module) = field(node, "source", source) {
                        result
                            .imports
                            .insert((line, module.trim_matches(['\'', '"']).to_owned()));
                    }
                }
                _ => {}
            }
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
        }
        Ok(result)
    }
}

fn rust_use_paths(
    node: Node<'_>,
    source: &str,
    prefix: &str,
    line: usize,
    out: &mut BTreeSet<(usize, String)>,
) {
    let join = |name: &str| {
        if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}::{name}")
        }
    };
    let path = match node.kind() {
        "self" if !prefix.is_empty() => prefix.to_owned(),
        "identifier" | "type_identifier" | "scoped_identifier" | "scoped_type_identifier" => {
            join(text(node, source))
        }
        "use_as_clause" => {
            if let Some(path) = node.child_by_field_name("path") {
                rust_use_paths(path, source, prefix, line, out);
            }
            return;
        }
        "scoped_use_list" => {
            if let (Some(path), Some(list)) = (
                field(node, "path", source),
                node.child_by_field_name("list"),
            ) {
                rust_use_paths(list, source, &join(path), line, out);
            }
            return;
        }
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                rust_use_paths(child, source, prefix, line, out);
            }
            return;
        }
        "use_wildcard" => {
            if let Some(path) = field(node, "path", source) {
                join(path)
            } else if !prefix.is_empty() {
                prefix.to_owned()
            } else {
                return;
            }
        }
        _ => return,
    };
    out.insert((line, path));
}

pub(super) fn import_target(
    file: &str,
    module: &str,
    language: &str,
    paths: &BTreeSet<String>,
) -> Option<String> {
    let parent = Path::new(file).parent()?;
    let mut candidates = Vec::new();
    if language == "python" {
        let module_name = module.trim_start_matches('.');
        let depth = module.len() - module_name.len();
        let mut dir = if depth == 0 {
            PathBuf::new()
        } else {
            parent.to_owned()
        };
        for _ in 1..depth {
            if !dir.pop() {
                return None;
            }
        }
        if !module_name.is_empty() {
            dir.extend(module_name.split('.'));
        }
        candidates.push(dir.with_extension("py"));
        candidates.push(dir.join("__init__.py"));
    } else if module.starts_with("./") || module.starts_with("../") {
        let mut dir = parent.to_owned();
        for part in Path::new(module).components() {
            match part {
                Component::CurDir => {}
                Component::ParentDir if dir.pop() => {}
                Component::Normal(name) => dir.push(name),
                _ => return None,
            }
        }
        candidates.push(dir.clone());
        if matches!(
            dir.extension().and_then(|s| s.to_str()),
            Some("js" | "jsx" | "mjs" | "cjs")
        ) {
            for extension in ["ts", "tsx", "mts", "cts"] {
                candidates.push(dir.with_extension(extension));
            }
        } else if dir.extension().is_none() {
            for extension in ["ts", "tsx", "js", "jsx", "mts", "cts", "mjs", "cjs"] {
                candidates.push(dir.with_extension(extension));
                candidates.push(dir.join("index").with_extension(extension));
            }
        }
    }
    candidates
        .into_iter()
        .filter_map(|p| {
            p.to_str()
                .map(|s| s.replace(std::path::MAIN_SEPARATOR, "/"))
        })
        .find(|p| paths.contains(p))
}

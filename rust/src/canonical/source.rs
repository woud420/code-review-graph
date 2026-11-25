#[path = "extra_edges.rs"]
mod extra_edges;
use super::imports::import_target;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

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
    pub import_details: super::imports::ImportDetails,
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
        if matches!(self.language, "lua" | "luau")
            && facts.symbols.iter().any(|symbol| {
                symbol
                    .name
                    .rsplit_once('.')
                    .is_some_and(|(_, name)| name == callee)
            })
        {
            return Some((
                callee.into(),
                format!("{}::{callee}", root.join(file).display()),
            ));
        }
        if self.language != "python" || !self.extra_edges.direct_name(callee) {
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
        let language = if crate::parser::bash::supports(path, source.as_bytes()) {
            Some("bash")
        } else {
            match path.extension().and_then(|s| s.to_str()) {
                Some("rb") => Some("ruby"),
                Some("tf" | "hcl") => Some("hcl"),
                Some("yaml" | "yml" | "properties") => Some("yaml"),
                Some(ext) if crate::parser::sql::supports(ext) => Some("sql"),
                Some(ext) if crate::parser::cfamily::supports(ext) => Some("c"),
                Some("lua") => Some("lua"),
                Some("luau") => Some("luau"),
                Some("php") => Some("php"),
                Some("sol") => Some("solidity"),
                Some("cs") => Some("csharp"),
                Some("java") => Some("java"),
                Some("go") => Some("go"),
                Some("json") => Some("json"),
                _ => None,
            }
        };
        if let Some(language) = language {
            return Ok(Self {
                language,
                lines: source.lines().count()
                    + usize::from(source.is_empty() || source.ends_with('\n')),
                declarations: Default::default(),
                imports: Default::default(),
                extra_edges: Default::default(),
                import_details: Default::default(),
            });
        }
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
            import_details: Default::default(),
            extra_edges: extra_edges::ExtraEdges::collect(tree.root_node(), source, language),
            language,
            lines: source.bytes().filter(|b| *b == b'\n').count() + 1,
            declarations: BTreeMap::new(),
            imports: BTreeSet::new(),
        };
        let mut pending = vec![tree.root_node()];
        while let Some(node) = pending.pop() {
            let line = node.start_position().row + 1;
            result.import_details.observe(node, source, language);
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
                        let mut decorators = Vec::new();
                        if language == "rust" {
                            for attr in std::iter::successors(node.prev_named_sibling(), |n| {
                                n.prev_named_sibling()
                            })
                            .take_while(|n| n.kind() == "attribute_item")
                            {
                                if let Some(value) =
                                    attr.named_child(0).and_then(|n| n.named_child(0))
                                {
                                    decorators.push(text(value, source).to_owned());
                                }
                            }
                            decorators.reverse();
                        }
                        result.declarations.insert(
                            (line, name.to_owned()),
                            Declaration {
                                params: field(node, "parameters", source).map(str::to_owned),
                                returns: if language == "rust" {
                                    None
                                } else {
                                    field(node, "return_type", source).map(str::to_owned)
                                },
                                modifiers: if decorators.is_empty() {
                                    is_async.then(|| "async".into())
                                } else {
                                    Some(decorators.join(","))
                                },
                                doc,
                                decorators,
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

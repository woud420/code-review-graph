mod extract;
mod grammar;

use crate::model::ParsedFile;
use anyhow::{Context, Result};
use std::{collections::HashMap, path::Path};
use toml_edit::TableLike;
use tree_sitter::Parser;

#[derive(Default)]
pub(crate) struct Registry {
    rules: Vec<Language>,
    extensions: HashMap<String, usize>,
}
struct Language {
    name: String,
    grammar: tree_sitter::Language,
    functions: Vec<String>,
    classes: Vec<String>,
    imports: Vec<String>,
    calls: Vec<String>,
    names: Vec<String>,
}

fn strings(table: &dyn TableLike, key: &str) -> Option<Vec<String>> {
    let Some(value) = table.get(key) else {
        return Some(Vec::new());
    };
    if key == "name_field"
        && let Some(value) = value.as_str()
    {
        return (!value.trim().is_empty()).then(|| vec![value.trim().into()]);
    }
    value
        .as_array()?
        .iter()
        .map(|e| {
            e.as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .collect()
}

impl Registry {
    pub(crate) fn load(root: &Path) -> Self {
        let mut registry = Self::default();
        let p = root.join(".code-review-graph/languages.toml");
        let Ok(text) = std::fs::read_to_string(p) else {
            return registry;
        };
        let Ok(document) = text.parse::<toml_edit::DocumentMut>() else {
            return registry;
        };
        let Some(entries) = document.get("languages").and_then(|v| v.as_table_like()) else {
            return registry;
        };
        for (name, item) in entries.iter() {
            if registry.rules.len() >= 20 {
                break;
            }
            if let Some((language, extensions)) =
                item.as_table_like().and_then(|t| registry.valid(name, t))
            {
                for extension in extensions {
                    registry.extensions.insert(extension, registry.rules.len());
                }
                registry.rules.push(language);
            }
        }
        registry
    }
    fn valid(&self, name: &str, table: &dyn TableLike) -> Option<(Language, Vec<String>)> {
        if !(1..=32).contains(&name.len())
            || !name.as_bytes()[0].is_ascii_lowercase()
            || !name
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
            || grammar::builtin_name(name)
        {
            return None;
        }
        let grammar = grammar::get(table.get("grammar")?.as_str()?.trim())?;
        let mut extensions = strings(table, "extensions")?;
        if extensions.is_empty() {
            return None;
        }
        for ext in &mut extensions {
            *ext = ext.strip_prefix('.')?.to_ascii_lowercase();
            if ext.is_empty()
                || ext.len() > 15
                || !ext
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'+' | b'-'))
                || grammar::builtin_extension(ext)
                || self.extensions.contains_key(ext)
            {
                return None;
            }
        }
        let language = Language {
            name: name.into(),
            grammar,
            functions: strings(table, "function_node_types")?,
            classes: strings(table, "class_node_types")?,
            imports: strings(table, "import_node_types")?,
            calls: strings(table, "call_node_types")?,
            names: strings(table, "name_field")?,
        };
        if (language.functions.is_empty()
            && language.classes.is_empty()
            && language.calls.is_empty()
            && language.imports.is_empty())
            || language.names.len() > 8
        {
            return None;
        }
        Some((language, extensions))
    }
    fn find(&self, path: &Path) -> Option<&Language> {
        if self.extensions.is_empty() {
            return None;
        }
        self.rules.get(
            *self
                .extensions
                .get(&path.extension()?.to_str()?.to_ascii_lowercase())?,
        )
    }
    pub(crate) fn accepts_file_language(&self, name: &str) -> bool {
        matches!(name, "blade" | "ansible" | "spring_config")
            || grammar::builtin_name(name)
            || self.rules.iter().any(|rule| rule.name == name)
    }
    pub(crate) fn supports(&self, path: &Path) -> bool {
        self.find(path).is_some()
    }
    pub(crate) fn parse(&self, path: &Path, source: &str) -> Option<Result<ParsedFile>> {
        let language = self.find(path)?;
        Some((|| {
            let mut parser = Parser::new();
            parser.set_language(&language.grammar)?;
            let tree = parser
                .parse(source, None)
                .context("parsing custom language")?;
            let mut file = ParsedFile::default();
            file.syntax.custom_language = Some(language.name.clone());
            language.extract(tree.root_node(), source, &mut file);
            super::syntax::resolve_local_calls(&mut file);
            Ok(file)
        })())
    }
}

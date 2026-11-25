use crate::model::json::PathInfo;
use crate::model::{ParsedFile, Symbol};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use tree_sitter::Parser;

fn value_type(kind: &str) -> Option<&str> {
    match kind {
        "true" | "false" => Some("boolean"),
        "array" | "object" | "string" | "number" | "null" => Some(kind),
        _ => None,
    }
}

pub(super) fn child_path(prefix: &str, key: &str) -> String {
    let mut chars = key.chars();
    if chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        format!("{prefix}.{key}")
    } else {
        format!("{prefix}[{}]", Value::from(key))
    }
}

fn locale_tag(value: &str) -> bool {
    let mut parts = value.split(['-', '_', '@']);
    let lang = parts.next().unwrap_or("");
    include_str!("locale-codes.txt")
        .split_whitespace()
        .any(|l| l == lang)
        && parts.all(|p| (2..=8).contains(&p.len()) && p.bytes().all(|c| c.is_ascii_alphanumeric()))
}

pub(super) fn file_class(path: &Path) -> Option<&'static str> {
    let name = path.file_stem()?.to_str()?.to_ascii_lowercase();
    let parent = path.parent()?;
    let folders: Vec<String> = parent
        .iter()
        .filter_map(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .collect();
    let locale = |dir: &str| {
        matches!(
            dir.trim_start_matches('_'),
            "i18n" | "l10n" | "locale" | "locales" | "translations"
        )
    };
    if folders.iter().any(|d| locale(d))
        && (name.split('.').any(locale_tag)
            || folders
                .windows(2)
                .any(|w| locale(&w[0]) && locale_tag(w[1].trim_start_matches('_'))))
    {
        return Some("json_locale_file");
    }
    if folders.iter().any(|s| {
        matches!(
            s.as_str(),
            "__fixtures__"
                | "__mocks__"
                | "__snapshots__"
                | "data"
                | "datasets"
                | "demo"
                | "demos"
                | "doc"
                | "docs"
                | "example"
                | "examples"
                | "fixtures"
                | "mocks"
                | "sample"
                | "samples"
                | "snapshots"
                | "test-data"
                | "test_data"
                | "testdata"
                | "test"
                | "tests"
                | "__tests__"
        )
    }) || name.starts_with("test_")
        || name.ends_with("_test")
        || name.ends_with(".test")
        || name.ends_with(".spec")
    {
        return Some("json_data_file");
    }
    None
}

pub fn parse(path: &Path, source: &str) -> Result<ParsedFile> {
    let mut facts = ParsedFile::default();
    let data = &mut facts.json;
    data.file = json!({"config_format":"json","json_truncated":false,"values_indexed":false});
    if let Some(reason) = file_class(path) {
        data.file[reason] = true.into();
        return Ok(facts);
    }
    if source.len() > 262_144 {
        data.file["json_truncated"] = true.into();
        return Ok(facts);
    }
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_json::LANGUAGE.into())?;
    let tree = parser.parse(source, None).context("parsing JSON")?;
    let root = tree.root_node();
    let mut queue = VecDeque::new();
    for n in root
        .named_children(&mut root.walk())
        .filter(|n| value_type(n.kind()).is_some())
    {
        queue.push_back((
            n,
            "$".to_owned(),
            "$".to_owned(),
            None,
            0,
            n.start_position().row + 1,
        ));
    }
    let mut indices = BTreeMap::new();
    while let Some((node, exact, schema, parent, depth, start)) = queue.pop_front() {
        let Some(ty) = value_type(node.kind()) else {
            continue;
        };
        let end = node.end_position().row + 1;
        let id = format!("json:{schema}");
        if let Some(&index) = indices.get(&id) {
            let sym: &mut Symbol = &mut facts.symbols[index];
            sym.start_line = sym.start_line.min(start);
            sym.end_line = sym.end_line.max(end);
            let extra = &mut data.paths.get_mut(&id).expect("indexed JSON path").extra;
            extra["occurrence_count"] = (extra["occurrence_count"].as_u64().unwrap() + 1).into();
            if extra["line_samples"].as_array().unwrap().len() < 50 {
                extra["line_samples"]
                    .as_array_mut()
                    .unwrap()
                    .push(start.into());
                extra["range_samples"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!([start, end]));
            } else {
                extra["lines_truncated"] = true.into();
            }
            let types = extra["value_types"].as_array_mut().unwrap();
            if !types.iter().any(|v| v.as_str() == Some(ty)) {
                types.push(ty.into());
                types.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
                extra["value_type"] = "mixed".into();
            }
        } else {
            if facts.symbols.len() >= 1_000 || depth > 32 || schema.chars().count() > 512 {
                data.file["json_truncated"] = true.into();
                continue;
            }
            indices.insert(id.clone(), facts.symbols.len());
            facts.symbols.push(Symbol {
                name: id.clone(),
                kind: "JsonPath".into(),
                start_line: start,
                end_line: end,
            });
            let extra = json!({"path":schema,"schema_path":schema,"example_path":exact.chars().take(512).collect::<String>(),
                "source_file":path.file_name().and_then(|s|s.to_str()),"value_type":ty,"value_types":[ty],"occurrence_count":1,
                "line_samples":[start],"range_samples":[[start,end]],"lines_truncated":false});
            data.paths.insert(id.clone(), PathInfo { parent, extra });
        }
        if ty == "object" {
            let pairs: Vec<_> = node
                .named_children(&mut node.walk())
                .filter(|n| n.kind() == "pair")
                .collect();
            let table = pairs.len() > 200;
            for pair in pairs {
                let (Some(key), Some(value)) = (
                    pair.child_by_field_name("key"),
                    pair.child_by_field_name("value"),
                ) else {
                    continue;
                };
                let Ok(key) = serde_json::from_str::<String>(&source[key.byte_range()]) else {
                    continue;
                };
                let next = if table {
                    format!("{schema}.*")
                } else {
                    child_path(&schema, &key)
                };
                queue.push_back((
                    value,
                    child_path(&exact, &key),
                    next,
                    Some(id.clone()),
                    depth + 1,
                    pair.start_position().row + 1,
                ));
            }
        } else if ty == "array" {
            for (i, item) in node
                .named_children(&mut node.walk())
                .filter(|n| value_type(n.kind()).is_some())
                .enumerate()
            {
                queue.push_back((
                    item,
                    format!("{exact}[{i}]"),
                    format!("{schema}[*]"),
                    Some(id.clone()),
                    depth + 1,
                    item.start_position().row + 1,
                ));
            }
        }
    }
    Ok(facts)
}

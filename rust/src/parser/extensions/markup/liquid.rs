use crate::model::{
    ParsedFile, Symbol,
    syntax::{Definition, Edge},
};
use regex::Regex;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
    sync::LazyLock,
};
type Items = BTreeMap<String, (&'static str, Vec<usize>, usize)>;
fn clean(range: &mut [u8]) {
    for ch in range {
        if !matches!(*ch, b'\n' | b'\r') {
            *ch = b' ';
        }
    }
}
fn statement(
    file: &mut ParsedFile,
    items: &mut Items,
    directive: &str,
    arguments: &str,
    line: usize,
) {
    static ASSIGN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*([a-zA-Z_][A-Za-z0-9_-]*)\s*=").unwrap());
    if directive == "assign"
        && let Some(cap) = ASSIGN.captures(arguments)
    {
        items
            .entry(cap[1].into())
            .or_insert(("Variable", vec![], line))
            .1
            .push(line);
    }
    if matches!(directive, "render" | "include") {
        let raw = arguments.trim_start();
        if let Some(q) = raw.chars().next()
            && matches!(q, '"' | '\'')
            && let Some((name, _)) = raw[1..].split_once(q)
            && !name.is_empty()
        {
            file.syntax.edges.push(Edge {
                kind: "IMPORTS_FROM".into(),
                owner: "<module>".into(),
                target: name.into(),
                line,
                extra: json!({"liquid_directive":directive}),
            });
        }
    }
}
pub(super) fn parse(s: &str) -> ParsedFile {
    static TAG: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)\{%-?\s*([#A-Za-z_][A-Za-z0-9_-]*)(.*?)-?%\}").unwrap());
    static OUT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)\{\{-?\s*(.*?)\s*-?\}\}").unwrap());
    let mut file = ParsedFile::default();
    let mut masked = s.as_bytes().to_vec();
    let mut literal = None;
    let mut items = Items::new();
    let starts: Vec<_> = std::iter::once(0)
        .chain(s.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let line = |at| starts.partition_point(|&i| i <= at);
    for cap in TAG.captures_iter(s) {
        let tag = cap.get(0).unwrap();
        let directive = cap.get(1).unwrap().as_str();
        let args = cap.get(2).unwrap();
        if let Some((kind, start)) = literal {
            if directive.strip_prefix("end") == Some(kind) {
                clean(&mut masked[start..tag.end()]);
                literal = None;
            }
            continue;
        }
        if matches!(
            directive,
            "comment" | "raw" | "doc" | "schema" | "javascript" | "stylesheet"
        ) {
            literal = Some((directive, tag.start()));
            continue;
        }
        clean(&mut masked[tag.range()]);
        if directive == "liquid" {
            let mut comment = false;
            for (i, row) in args.as_str().split('\n').enumerate() {
                let row = row.trim();
                let (k, v) = row.split_once(char::is_whitespace).unwrap_or((row, ""));
                match k {
                    "comment" => comment = true,
                    "endcomment" => comment = false,
                    _ if !comment => statement(&mut file, &mut items, k, v, line(args.start()) + i),
                    _ => {}
                }
            }
        } else {
            statement(
                &mut file,
                &mut items,
                directive,
                args.as_str(),
                line(args.start()),
            );
        }
    }
    let masked = String::from_utf8(masked).unwrap();
    for cap in OUT.captures_iter(&masked) {
        let expr = cap[1].split_whitespace().collect::<Vec<_>>().join(" ");
        if expr.is_empty() {
            continue;
        }
        let text = if expr.chars().count() > 120 {
            format!(
                "{}#{}",
                expr.chars().take(120).collect::<String>(),
                hex::encode(&Sha256::digest(expr.as_bytes())[..4])
            )
        } else {
            expr
        };
        let range = cap.get(0).unwrap();
        items
            .entry(format!("output:{text}"))
            .or_insert(("Output", vec![], line(range.end())))
            .1
            .push(line(range.start()));
    }
    for (name, (kind, lines, end)) in items {
        file.symbols.push(Symbol {
            name: name.clone(),
            kind: kind.into(),
            start_line: lines[0],
            end_line: end,
        });
        file.syntax.display_names.insert(name.clone(), name.clone());
        file.syntax.declarations.insert(
            name,
            Definition {
                extra: if lines.len() > 1 {
                    json!({"lines":lines})
                } else {
                    json!({})
                },
                ..Default::default()
            },
        );
    }
    file
}
pub(crate) fn snippet(
    root: &Path,
    relative: &str,
    name: &str,
    files: &BTreeSet<String>,
) -> Option<String> {
    if Path::new(name)
        .components()
        .any(|v| !matches!(v, Component::Normal(_)))
    {
        return None;
    }
    let leaf = if name.ends_with(".liquid") {
        name.into()
    } else {
        format!("{name}.liquid")
    };
    for dir in Path::new(relative).ancestors().skip(1) {
        let file = dir.join("snippets").join(&leaf);
        if files.contains(file.to_str()?) {
            return Some(root.join(file).to_string_lossy().into_owned());
        }
    }
    None
}

use crate::model::{ParsedFile, syntax::Edge};
use regex::Regex;
use serde_json::json;
use std::{path::Path, sync::LazyLock};

pub(crate) fn matches(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().ends_with(".blade.php"))
}

pub(super) fn parse(source: &str) -> ParsedFile {
    static COMMENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)\{\{--.*?(?:--\}\}|$)").unwrap());
    static DIRECTIVE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"@(extends|include|component|livewire)\s*\(\s*(?:"([^'"]+)"|'([^'"]+)')\s*\)"#)
            .unwrap()
    });
    let mut cleaned = source.as_bytes().to_vec();
    for comment in COMMENT.find_iter(source) {
        for byte in &mut cleaned[comment.range()] {
            if !matches!(*byte, b'\r' | b'\n') {
                *byte = b' ';
            }
        }
    }
    let text = std::str::from_utf8(&cleaned).expect("masking complete comments preserves UTF-8");
    let lines: Vec<_> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let mut file = ParsedFile::default();
    for cap in DIRECTIVE.captures_iter(text) {
        let start = cap.get(0).unwrap().start();
        if text[..start].ends_with('@') {
            continue;
        }
        let directive = &cap[1];
        let target = cap.get(2).or_else(|| cap.get(3)).unwrap().as_str();
        file.syntax.edges.push(Edge {
            kind: if directive == "livewire" {
                "REFERENCES"
            } else {
                "IMPORTS_FROM"
            }
            .into(),
            owner: "<module>".into(),
            target: target.into(),
            line: lines.partition_point(|i| *i <= start),
            extra: json!({"blade_directive":directive}),
        });
    }
    file
}

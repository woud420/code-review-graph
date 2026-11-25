use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

pub(super) struct Cell {
    pub index: usize,
    pub language: String,
    pub source: String,
}
pub(super) fn jupyter(s: &str) -> Option<(String, Vec<Cell>)> {
    let value: Value = serde_json::from_str(s).ok()?;
    let meta = &value["metadata"];
    let kernel = meta["kernelspec"]["language"]
        .as_str()
        .or(meta["language_info"]["name"].as_str())
        .unwrap_or("python")
        .to_lowercase();
    if !matches!(kernel.as_str(), "r" | "python") {
        return None;
    }
    let mut cells = Vec::new();
    for (index, cell) in value["cells"].as_array().into_iter().flatten().enumerate() {
        if cell["cell_type"] != "code" {
            continue;
        }
        let mut lines: Vec<&str> = match &cell["source"] {
            Value::String(v) => v.split_inclusive('\n').collect(),
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => continue,
        };
        let Some(first) = lines.first() else {
            continue;
        };
        let mut language = kernel.as_str();
        if let Some(magic) = first.trim().split(' ').next() {
            match magic {
                "%md" | "%scala" | "%sh" => continue,
                "%sql" | "%r" | "%python" => {
                    language = &magic[1..];
                    lines.remove(0);
                }
                _ => {}
            }
        }
        if language != "sql" {
            lines.retain(|l| !l.trim_start().starts_with(['%', '!']));
        }
        if !lines.is_empty() {
            cells.push(Cell {
                index,
                language: language.into(),
                source: lines.join(""),
            });
        }
    }
    Some((kernel, cells))
}
pub(super) fn databricks(source: &str) -> Vec<Cell> {
    static BOUNDARY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^# COMMAND\s*-+\s*$").unwrap());
    let lines: Vec<_> = source.split('\n').skip(1).collect();
    let mut cells = Vec::new();
    for (index, lines) in lines.split(|l| BOUNDARY.is_match(l)).enumerate() {
        let mut present = lines.iter().copied().filter(|l| !l.trim().is_empty());
        let Some(first) = present.next() else {
            continue;
        };
        let magic = first.starts_with("# MAGIC ") && present.all(|l| l.starts_with("# MAGIC "));
        if magic && (first.starts_with("# MAGIC %md") || first.starts_with("# MAGIC %sh")) {
            continue;
        }
        let language = if magic && first.starts_with("# MAGIC %sql") {
            "sql"
        } else if magic && first.starts_with("# MAGIC %r") {
            "r"
        } else {
            "python"
        };
        let source = if language == "python" {
            lines
                .iter()
                .copied()
                .filter(|l| !l.starts_with("# MAGIC "))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            lines
                .iter()
                .copied()
                .filter(|l| *l != first)
                .map(|l| l.strip_prefix("# MAGIC ").unwrap_or(l))
                .collect::<Vec<_>>()
                .join("\n")
        };
        cells.push(Cell {
            index,
            language: language.into(),
            source,
        });
    }
    cells
}

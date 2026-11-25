use super::config::{Config, Embeddings};
use crate::cli::{search, wire::value_repr};
use anyhow::Result;
use rusqlite::{Connection, Row};
use serde_json::{Value, json};
use std::{fs, path::Path};

const STOPWORDS: &[&str] = &[
    "how", "does", "do", "the", "a", "an", "is", "are", "was", "what", "where", "when", "which",
    "who", "why", "and", "or", "in", "on", "of", "to", "for", "with", "via", "into", "from",
    "this", "that", "it", "its",
];
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "__pycache__",
    ".code-review-graph",
    ".venv",
    "venv",
    "dist",
    "build",
];

fn terms(question: &str) -> Vec<String> {
    let mut words = search::identifiers(question);
    for word in question.split_whitespace() {
        let word = word
            .trim_matches(|c| ".,;:!?\"'()[]{}`".contains(c))
            .to_lowercase();
        if word.chars().count() >= 3
            && !STOPWORDS.contains(&word.as_str())
            && !words.contains(&word)
        {
            words.push(word);
        }
    }
    words
}

fn estimate(text: &str) -> usize {
    (text.chars().count() / 4).max(1)
}

fn top_files(root: &Path, terms: &[String], limit: i64) -> Vec<(String, usize)> {
    if terms.is_empty() {
        return vec![];
    }
    let test_intent = terms
        .iter()
        .any(|s| search::TEST_INTENT.contains(&s.as_str()));
    let mut files = Vec::new();
    let mut walk = ignore::WalkBuilder::new(root);
    walk.standard_filters(false).filter_entry(|e| {
        !e.file_name()
            .to_str()
            .is_some_and(|p| SKIP_DIRS.contains(&p))
    });
    for entry in walk.build().flatten() {
        let path = entry.path();
        if !matches!(
            path.extension().and_then(|e| e.to_str()),
            Some(
                "py" | "js"
                    | "ts"
                    | "tsx"
                    | "go"
                    | "rs"
                    | "java"
                    | "c"
                    | "cpp"
                    | "h"
                    | "rb"
                    | "php"
                    | "swift"
                    | "kt"
            )
        ) || !path.is_file()
            || path.is_symlink() && !path.canonicalize().is_ok_and(|p| p.starts_with(root))
        {
            continue;
        }
        let Ok(bytes) = fs::read(path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes)
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        let lower = text.to_lowercase();
        let count: usize = terms.iter().map(|t| lower.matches(t).count()).sum();
        if count == 0 {
            continue;
        }
        let normalized = path.strip_prefix(root).unwrap_or(path);
        let score = if !test_intent && search::test_file(root, normalized) {
            count as f64 * 0.4
        } else {
            count as f64
        };
        files.push((
            score,
            normalized.to_string_lossy().into_owned(),
            estimate(&text),
        ));
    }
    files.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let take = if limit < 0 {
        files.len().saturating_sub(limit.unsigned_abs() as usize)
    } else {
        limit as usize
    };
    files
        .into_iter()
        .take(take)
        .map(|(_, p, n)| (p, n))
        .collect()
}

fn edge_text(r: &Row<'_>) -> rusqlite::Result<String> {
    let field = |key: &str| -> rusqlite::Result<Value> {
        Ok(match key {
            "id" | "line" => json!(r.get::<_, Option<i64>>(key)?),
            "confidence" => json!(r.get::<_, Option<f64>>(key)?),
            "extra" => r
                .get::<_, Option<String>>(key)?
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_else(|| json!({})),
            _ => json!(r.get::<_, Option<String>>(key)?),
        })
    };
    let mut fields = Vec::new();
    for key in [
        "id",
        "kind",
        "source_qualified",
        "target_qualified",
        "file_path",
        "line",
        "extra",
        "confidence",
        "confidence_tier",
        "target_resolution",
    ] {
        fields.push(format!("{key}={}", value_repr(&field(key)?)));
    }
    Ok(format!("GraphEdge({})", fields.join(", ")))
}

pub(super) fn run(
    db: &Connection,
    path: &Path,
    cfg: &Config,
    options: &Embeddings,
) -> Result<Vec<Value>> {
    let questions: Vec<_> = if cfg.agent_questions.is_empty() {
        cfg.search_queries.iter().map(|q| &q.query).collect()
    } else {
        cfg.agent_questions.iter().collect()
    };
    let mut rows = Vec::new();
    let mut edges =
        db.prepare("SELECT * FROM edges WHERE source_qualified=? ORDER BY id LIMIT 5")?;
    for question in questions {
        let terms = terms(question);
        let top = top_files(path, &terms, cfg.agent_baseline_top_k.unwrap_or(3));
        let baseline = top.iter().map(|(_, n)| n).sum::<usize>();
        let mut row = json!({"repo":cfg.name,"question":question,"terms":terms.join(" "),"files_matched":top.len(),
            "top_files":top.iter().map(|(p,_)|p.as_str()).collect::<Vec<_>>().join(";"),"baseline_tokens":baseline,
            "graph_tokens":"","baseline_to_graph_ratio":"","status":"ok","error":""});
        let params = search::Options {
            query: question.clone(),
            provider: options.embed_provider.clone().filter(|_| options.embed),
            model: options.embed_model.clone().filter(|_| options.embed),
            ..Default::default()
        };
        match search::ranked(db, path, &params, 5) {
            Ok((_, hits)) => {
                let mut tokens = 0;
                for hit in hits {
                    tokens += estimate(&value_repr(&hit));
                    for edge in edges
                        .query_map([hit["qualified_name"].as_str().unwrap_or("")], edge_text)?
                    {
                        tokens += estimate(&edge?);
                    }
                }
                row["graph_tokens"] = tokens.into();
                if tokens == 0 {
                    row["status"] = "no_graph_results".into();
                } else if baseline == 0 {
                    row["status"] = "no_baseline_match".into();
                } else {
                    row["baseline_to_graph_ratio"] =
                        format!("{:.1}", baseline as f64 / tokens as f64)
                            .parse::<f64>()?
                            .into();
                }
            }
            Err(err) => {
                eprintln!("WARNING: hybrid_search failed on {question:?}: {err}");
                row["status"] = "error".into();
                row["error"] = err.to_string().chars().take(200).collect::<String>().into();
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

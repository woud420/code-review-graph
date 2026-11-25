use super::config::{Commit, Config};
use crate::cli::{
    changes::git,
    reports::{detect, review},
    wire::json_text,
};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path};

fn diff(root: &Path, sha: &str, names: bool) -> Result<String> {
    let parent = format!("{sha}~1");
    for (old, new) in [(parent.as_str(), sha), ("HEAD~1", "HEAD")] {
        let mut args = vec!["diff"];
        if names {
            args.push("--name-only");
        }
        args.extend([old, new, "--"]);
        let (ok, bytes) = git(root, &args)?;
        if ok {
            return Ok(text(&bytes));
        }
    }
    Ok(String::new())
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

fn tokens(s: &str) -> usize {
    s.chars().count() / 4
}

fn token_row(
    db: &Connection,
    root: &Path,
    cfg: &Config,
    commit: &Commit,
    files: &[String],
) -> Result<Value> {
    let naive: usize = files
        .iter()
        .filter_map(|p| fs::read(root.join(p)).ok())
        .map(|v| tokens(&text(&v)))
        .sum();
    let standard = tokens(&diff(root, &commit.sha, false)?);
    let mut row = json!({"repo":cfg.name,"commit":commit.sha,"description":commit.description,"changed_files":files.len(),"naive_tokens":naive,"standard_tokens":standard,"graph_tokens":"","naive_to_graph_ratio":"","standard_to_graph_ratio":"","status":"ok","error":""});
    match review::run(db, root, &json!({"changed_files":files})) {
        Ok(report) => {
            let count = tokens(&json_text(&report, false));
            let ratio = |n: usize| super::round(n as f64 / count.max(1) as f64, 1);
            row["graph_tokens"] = json!(count);
            row["naive_to_graph_ratio"] = json!(ratio(naive));
            row["standard_to_graph_ratio"] = json!(ratio(standard));
        }
        Err(err) => record_error(&mut row, err),
    }
    Ok(row)
}

fn neighbors(db: &Connection, files: &BTreeSet<String>) -> Result<BTreeSet<String>> {
    let mut query=db.prepare("SELECT DISTINCT e.source_qualified FROM nodes n JOIN edges e ON e.target_qualified=n.qualified_name WHERE n.file_path IN (SELECT value FROM json_each(?1)) AND e.kind IN ('CALLS','IMPORTS_FROM')")?;
    let mut found = BTreeSet::new();
    for row in query.query_map([serde_json::to_string(files)?], |r| r.get::<_, String>(0))? {
        let source = row?;
        let file = source.split("::").next().unwrap_or_default();
        if file.contains('/') {
            found.insert(file.to_string());
        }
    }
    Ok(found)
}

fn predictions(
    db: &Connection,
    root: &Path,
    files: &BTreeSet<String>,
    base: Option<&str>,
) -> Result<BTreeSet<String>> {
    let report = detect::analysis(
        db,
        root,
        &files.iter().cloned().collect::<Vec<_>>(),
        base,
        false,
    )?;
    let mut paths = BTreeSet::new();
    for node in report["changed_functions"].as_array().into_iter().flatten() {
        if let Some(p) = node["file_path"].as_str().or_else(|| node["file"].as_str()) {
            paths.insert(p.to_owned());
        }
    }
    for flow in report["affected_flows"].as_array().into_iter().flatten() {
        for node in flow["nodes"].as_array().into_iter().flatten() {
            if let Some(p) = node["file_path"].as_str() {
                paths.insert(p.to_owned());
            }
        }
    }
    Ok(paths)
}

fn record_error(row: &mut Value, error: anyhow::Error) {
    row["status"] = json!("error");
    row["error"] = json!(error.to_string().chars().take(200).collect::<String>());
}

fn score(row: &mut Value, predicted: &BTreeSet<String>, actual: &BTreeSet<String>) {
    let tp = predicted.intersection(actual).count();
    let precision = tp as f64 / predicted.len().max(1) as f64;
    let recall = tp as f64 / actual.len().max(1) as f64;
    let rounded = |n: f64| super::round(n, 3);
    row["predicted_files"] = json!(predicted.len());
    row["actual_files"] = json!(actual.len());
    row["true_positives"] = json!(tp);
    row["precision"] = json!(rounded(precision));
    row["recall"] = json!(rounded(recall));
    row["f1"] = json!(rounded(
        2. * precision * recall / (precision + recall).max(0.001)
    ));
}

fn impact_rows(
    db: &Connection,
    root: &Path,
    cfg: &Config,
    commit: &Commit,
    files: &[String],
) -> Result<[Value; 2]> {
    let abs: BTreeSet<String> = files
        .iter()
        .map(|p| root.join(p).to_string_lossy().into_owned())
        .collect();
    let row = |mode: &str, seed: &str| json!({"repo":cfg.name,"commit":commit.sha,"ground_truth_mode":mode,"seed_file":seed,"predicted_files":"","actual_files":"","true_positives":"","precision":"","recall":"","f1":"","status":"ok","error":""});
    let mut graph = row("graph-derived (circular — upper bound)", "");
    match predictions(db, root, &abs, Some(&format!("{}~1", commit.sha))) {
        Ok(mut predicted) => {
            predicted.extend(abs.iter().cloned());
            let mut actual = neighbors(db, &abs)?;
            actual.extend(abs.iter().cloned());
            score(&mut graph, &predicted, &actual);
        }
        Err(e) => record_error(&mut graph, e),
    }
    let mut others = abs;
    let seed = others.pop_first().expect("non-empty changed file list");
    let rel = Path::new(&seed).strip_prefix(root)?.to_string_lossy();
    let mut co = row("co-change (same commit, seed excluded)", &rel);
    if others.is_empty() {
        co["status"] = json!("skipped");
        co["error"] = json!("single-file commit: no co-changed files to grade against");
    } else {
        let seeds = BTreeSet::from([seed.clone()]);
        match predictions(db, root, &seeds, None) {
            Ok(mut predicted) => {
                predicted.extend(neighbors(db, &seeds)?);
                predicted.remove(&seed);
                score(&mut co, &predicted, &others);
            }
            Err(e) => record_error(&mut co, e),
        }
    }
    Ok([graph, co])
}

pub(super) fn run(
    db: &Connection,
    root: &Path,
    cfg: &Config,
    benchmark: &str,
) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    for commit in &cfg.test_commits {
        let files = diff(root, &commit.sha, true)?
            .lines()
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if files.is_empty() {
            continue;
        }
        if benchmark == "impact_accuracy" {
            rows.extend(impact_rows(db, root, cfg, commit, &files)?);
        } else {
            rows.push(token_row(db, root, cfg, commit, &files)?);
        }
    }
    Ok(rows)
}

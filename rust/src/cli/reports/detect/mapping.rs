use super::*;
use crate::cli::changes::{Diff, git, git_timed};
use std::collections::HashSet;

pub(super) fn churn(root: &Path, on: bool) -> (HashMap<String, usize>, &'static str) {
    let mut counts = HashMap::new();
    let days = std::env::var("CRG_CHURN_WINDOW_DAYS")
        .unwrap_or_else(|_| "90".into())
        .parse::<u32>()
        .unwrap_or(0);
    if !on || days == 0 {
        return (counts, "off");
    }
    let budget = std::env::var("CRG_CHURN_MAX_COMMITS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(2000);
    let result = git_timed(
        root,
        &[
            "-c",
            "core.quotepath=off",
            "log",
            &format!("--since={days}.days.ago"),
            &format!("--max-count={budget}"),
            "--numstat",
            "--no-renames",
            "--format=",
            "-z",
            "--",
        ],
        "CRG_CHURN_TIMEOUT",
        5.,
    );
    let Ok((true, bytes)) = result else {
        return (counts, "unavailable");
    };
    let output = String::from_utf8_lossy(&bytes);
    for row in output.split('\0') {
        let mut parts = row.splitn(3, '\t');
        let (Some(add), Some(remove), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if !path.is_empty()
            && [add, remove]
                .iter()
                .all(|n| *n == "-" || n.parse::<usize>().is_ok())
        {
            *counts
                .entry(root.join(path).to_string_lossy().into_owned())
                .or_insert(0) += 1;
        }
    }
    (counts, "ok")
}

pub(super) struct ChangeSet {
    pub nodes: Vec<(Value, Value)>,
    pub removed: Vec<Value>,
    pub unread: usize,
}

fn in_file(conn: &Connection, path: &Path) -> Result<Vec<(Value, Value)>> {
    Ok(conn
        .prepare("SELECT * FROM nodes WHERE file_path=?1")?
        .query_map([path.to_string_lossy()], |r| {
            Ok((
                node(r)?,
                serde_json::from_str(&r.get::<_, String>("extra")?).unwrap_or(Value::Null),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn overlaps(n: &Value, extra: &Value, ranges: &[(i64, i64)]) -> bool {
    if matches!(text(n, "kind"), "JsonPath" | "YamlPath")
        && extra["lines_truncated"] != true
        && let Some(samples) = extra["range_samples"].as_array()
    {
        return samples.iter().any(|row| {
            row[0]
                .as_i64()
                .zip(row[1].as_i64())
                .is_some_and(|(s, e)| ranges.iter().any(|&(a, b)| s <= b && e >= a))
        });
    }
    n["line_start"]
        .as_i64()
        .zip(n["line_end"].as_i64())
        .is_some_and(|(s, e)| ranges.iter().any(|&(a, b)| s <= b && e >= a))
}

pub(super) fn run(
    conn: &Connection,
    root: &Path,
    base: &str,
    files: &[String],
    diff: &[Diff],
    counts: &HashMap<String, usize>,
) -> Result<ChangeSet> {
    let mut nodes = Vec::new();
    let mut removed = Vec::new();
    let mut ghosts = Vec::new();
    let mut unread = 0;
    if diff.is_empty() {
        for f in files {
            nodes.extend(in_file(conn, &root.join(f))?);
        }
    } else {
        let mut seen = HashSet::new();
        let mut after = Vec::new();
        for file in diff {
            let path = root.join(&file.path);
            let current = in_file(conn, &path)?;
            let edits = file
                .hunks
                .iter()
                .filter(|h| h.count > 0)
                .map(|h| h.range())
                .collect::<Vec<_>>();
            let holes = file
                .hunks
                .iter()
                .filter(|h| h.count == 0)
                .collect::<Vec<_>>();
            for (n, extra) in &current {
                if overlaps(n, extra, &edits) {
                    nodes.push((n.clone(), extra.clone()));
                } else if !is_code(n) && holes.iter().any(|h| overlaps(n, extra, &[h.range()])) {
                    after.push((n.clone(), extra.clone()));
                }
            }
            if holes.is_empty() || current.is_empty() {
                continue;
            }
            let variant = format!("{base}:{}", file.old_path);
            let old = git(root, &["show", &variant])
                .ok()
                .filter(|(ok, _)| *ok)
                .and_then(|(_, bytes)| {
                    code_review_graph_rs::parse_file(&path, &String::from_utf8_lossy(&bytes)).ok()
                });
            let Some(old) = old else {
                unread += 1;
                for (n, extra) in &current {
                    if is_code(n)
                        && holes.iter().any(|h| {
                            n["line_start"].as_i64().unwrap_or(i64::MAX) <= h.start
                                && h.start < n["line_end"].as_i64().unwrap_or(0)
                        })
                    {
                        after.push((n.clone(), extra.clone()));
                    }
                }
                continue;
            };
            for symbol in old.symbols {
                let s = symbol.start_line as i64;
                let e = symbol.end_line as i64;
                let hit = holes
                    .iter()
                    .filter(|h| s < h.old_start + h.old_count && e >= h.old_start)
                    .collect::<Vec<_>>();
                if hit.is_empty() {
                    continue;
                }
                let qn = format!("{}::{}", path.display(), symbol.name);
                if let Some((n, extra)) = current.iter().find(|(n, _)| n["qualified_name"] == qn) {
                    if !hit
                        .iter()
                        .any(|h| h.old_start <= s && e < h.old_start + h.old_count)
                    {
                        after.push((n.clone(), extra.clone()));
                    }
                } else {
                    let calls = old
                        .calls
                        .iter()
                        .filter(|c| c.caller == symbol.name)
                        .map(|c| c.callee.clone())
                        .collect();
                    let (entry, ghost) = super::removed::entry(
                        conn,
                        &path,
                        &qn,
                        &symbol,
                        *counts.get(path.to_string_lossy().as_ref()).unwrap_or(&0),
                        calls,
                    )?;
                    if entry["used_by_total"] != 0 {
                        ghosts.push(ghost);
                    }
                    removed.push(entry);
                }
            }
        }
        nodes.append(&mut after);
        for f in files {
            let path = root.join(f);
            if diff.iter().any(|d| root.join(&d.path) == path) {
                continue;
            }
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            if git(
                root,
                &[
                    "--literal-pathspecs",
                    "ls-files",
                    "--error-unmatch",
                    "--",
                    &rel.to_string_lossy(),
                ],
            )
            .is_ok_and(|(ok, _)| !ok)
            {
                nodes.extend(in_file(conn, &path)?);
            }
        }
        nodes.retain(|(n, _)| seen.insert(text(n, "qualified_name").to_string()));
    }
    let flows = code_review_graph_rs::canonical::postprocess::removed_flow_scores(conn, &ghosts)?;
    for entry in &mut removed {
        if let Some(flow) = flows.get(text(entry, "qualified_name")) {
            let base = entry["risk_score"].as_f64().unwrap_or_default();
            entry["risk_score"] = json!(rounded((base + flow.min(0.25)).min(1.0)));
        }
    }
    removed.sort_by(|a, b| {
        b["risk_score"]
            .as_f64()
            .unwrap_or_default()
            .total_cmp(&a["risk_score"].as_f64().unwrap_or_default())
            .then_with(|| {
                b["used_by_total"]
                    .as_i64()
                    .cmp(&a["used_by_total"].as_i64())
            })
            .then_with(|| text(a, "file").cmp(text(b, "file")))
            .then_with(|| {
                a["old_line_start"]
                    .as_i64()
                    .cmp(&b["old_line_start"].as_i64())
            })
    });
    Ok(ChangeSet {
        nodes,
        removed,
        unread,
    })
}

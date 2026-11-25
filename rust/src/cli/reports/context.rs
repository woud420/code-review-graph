use super::*;
use crate::cli::{changes, store};
use sha2::{Digest, Sha256};

pub(in crate::cli) fn not_ready(reason: &str, summary: &str) -> Value {
    json!({"status":"not_ready","reason":reason,"summary":summary,"next_tool_suggestions":["build_or_update_graph_tool"]})
}

fn holds_commits(conn: &Connection, root: &Path, built: &str) -> Result<bool> {
    if built.is_empty() || built.starts_with('-') || !built.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(false);
    }
    let Ok((true, raw)) = changes::git(
        root,
        &[
            "diff",
            "--relative",
            "--name-only",
            "--no-renames",
            "-z",
            built,
            "HEAD",
            "--",
            ".",
        ],
    ) else {
        return Ok(false);
    };
    let paths: Vec<_> = raw.split(|b| *b == 0).filter(|s| !s.is_empty()).collect();
    if paths.len() > 200 {
        return Ok(false);
    }
    for rel in paths {
        let path = root.join(String::from_utf8_lossy(rel).as_ref());
        let hash = conn
            .query_row(
                "SELECT file_hash FROM nodes WHERE kind='File' AND file_path=?1",
                [path.to_string_lossy()],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        if let Some(hash) = hash {
            let Ok(bytes) = std::fs::read(&path) else {
                return Ok(false);
            };
            if hex::encode(Sha256::digest(bytes)) != hash {
                return Ok(false);
            }
        } else if path.exists() {
            let bytes = std::fs::read(&path)?;
            if code_review_graph_rs::parse_file(&path, &String::from_utf8_lossy(&bytes)).is_ok() {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

pub(super) fn run(conn: &Connection, root: &Path, args: &Value) -> Result<Value> {
    let counts = store::Counts::read(conn)?;
    if counts.nodes() == 0 {
        return Ok(not_ready(
            "empty_graph",
            "The graph database contains no nodes. Build the graph before requesting context.",
        ));
    }
    let provenance = store::provenance(conn, root)?;
    if provenance["head_matches_build"] == false
        && !holds_commits(conn, root, text(&provenance, "built_at_sha"))?
    {
        return Ok(not_ready(
            "stale_graph",
            "The graph was built at a different Git commit. Update it before requesting context.",
        ));
    }
    let task = text(args, "task").to_lowercase();
    let options = if ["review", "pr", "merge", "diff"]
        .iter()
        .any(|w| task.contains(w))
    {
        [
            "detect_changes_tool",
            "get_affected_flows_tool",
            "get_review_context_tool",
        ]
    } else if ["debug", "bug", "error", "fix"]
        .iter()
        .any(|w| task.contains(w))
    {
        [
            "semantic_search_nodes_tool",
            "query_graph_tool",
            "get_flow_tool",
        ]
    } else if ["refactor", "rename", "dead", "clean"]
        .iter()
        .any(|w| task.contains(w))
    {
        [
            "refactor_tool",
            "find_large_functions_tool",
            "get_architecture_overview_tool",
        ]
    } else if ["onboard", "understand", "explore", "arch"]
        .iter()
        .any(|w| task.contains(w))
    {
        [
            "get_architecture_overview_tool",
            "list_communities_tool",
            "list_flows_tool",
        ]
    } else {
        [
            "detect_changes_tool",
            "semantic_search_nodes_tool",
            "get_architecture_overview_tool",
        ]
    };
    let mut summary = vec![format!(
        "{} nodes, {} edges across {} files.",
        counts.nodes(),
        counts.edges(),
        counts.files()
    )];
    let mut selected = Vec::new();
    let mut risk = "unknown";
    let base = args["base"].as_str().unwrap_or("HEAD~1");
    let files = if args["changed_files"]
        .as_array()
        .is_some_and(|v| !v.is_empty())
    {
        changes::paths(root, args)
    } else {
        changes::discover(root, base)
    };
    match files.and_then(|files| Ok((files, changes::resolve_base(root, base)?))) {
        Err(e) => summary.push(format!("Degraded: {e}")),
        Ok((files, base)) if !files.is_empty() => {
            if let Ok(report) = super::detect::analysis(conn, root, &files, &base, true) {
                let score = report["risk_score"].as_f64().unwrap_or_default();
                risk = if score > 0.7 {
                    "high"
                } else if score > 0.4 {
                    "medium"
                } else {
                    "low"
                };
                summary.push(format!("Risk: {risk} ({score:.2})."));
                let gaps = report["test_gaps"].as_array().map_or(0, Vec::len);
                if gaps > 0 {
                    summary.push(format!("{gaps} test gaps."));
                }
                if report["churn_status"] == "unavailable" {
                    summary.push("Degraded: change-frequency risk unavailable (git history too slow); risk excludes churn.".into());
                }
                selected = report["changed_functions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(5)
                    .map(|n| n["name"].clone())
                    .collect();
            }
        }
        _ => {}
    }
    let mut out = json!({"status":"ok","summary":summary.join(" ")});
    if !selected.is_empty() {
        out["key_entities"] = json!(selected);
    }
    if risk != "unknown" {
        out["risk"] = json!(risk)
    }
    for (k, q) in [
        (
            "communities",
            "SELECT name FROM communities ORDER BY size DESC LIMIT 3",
        ),
        (
            "flows_affected",
            "SELECT name FROM flows ORDER BY criticality DESC LIMIT 3",
        ),
    ] {
        let names = conn
            .prepare(q)?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !names.is_empty() {
            out[k] = json!(names);
        }
    }
    out["next_tool_suggestions"] = json!(options);
    Ok(out)
}

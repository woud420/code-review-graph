mod details;
mod mapping;
mod removed;
mod render;

use super::review::risk;
use super::*;
use crate::cli::{changes, query::is_test};
use std::collections::{HashMap, HashSet};

fn rounded(n: f64) -> f64 {
    (n * 1e4).round_ties_even() / 1e4
}
fn churn_term(n: usize) -> f64 {
    (n as f64 / 10.).min(1.) * 0.15
}
fn is_code(node: &Value) -> bool {
    matches!(text(node, "kind"), "Function" | "Test" | "Class")
}
fn cap(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(default)
}
fn names(list: &[Value], coverage: &str) -> String {
    let mut seen = HashSet::new();
    list.iter()
        .filter(|g| g["coverage"] == coverage)
        .map(|g| text(g, "name"))
        .filter(|n| seen.insert(*n))
        .take(5)
        .collect::<Vec<_>>()
        .join(", ")
}

fn flows(conn: &Connection, root: &Path, files: &[String]) -> Result<Vec<Value>> {
    let abs: Vec<_> = files.iter().map(|f| root.join(f)).collect();
    let mut stmt=conn.prepare("SELECT * FROM flows WHERE id IN (SELECT m.flow_id FROM flow_memberships m JOIN nodes n ON n.id=m.node_id WHERE n.file_path IN(SELECT value FROM json_each(?1))) ORDER BY criticality DESC,id")?;
    let mut out = Vec::new();
    for flow in stmt.query_map([serde_json::to_string(&abs)?], flow_row)? {
        let mut f = flow?;
        let steps = flow_steps(conn, &f)?;
        let created = f
            .as_object_mut()
            .unwrap()
            .shift_remove("created_at")
            .unwrap_or(Value::Null);
        let updated = f
            .as_object_mut()
            .unwrap()
            .shift_remove("updated_at")
            .unwrap_or(Value::Null);
        f["steps"] = json!(steps);
        f["created_at"] = created;
        f["updated_at"] = updated;
        out.push(f);
    }
    Ok(out)
}

pub(super) fn analysis(
    conn: &Connection,
    root: &Path,
    files: &[String],
    base: &str,
    include_churn: bool,
) -> Result<Value> {
    let (counts, churn) = mapping::churn(root, include_churn);
    let mut unavailable = String::new();
    let diff = changes::read_diff(root, base).unwrap_or_else(|e| {
        unavailable = e.to_string();
        vec![]
    });
    let changes = mapping::run(conn, root, base, files, &diff, &counts)?;
    let mut funcs = Vec::new();
    let mut yaml = Vec::new();
    let mut json_paths = Vec::new();
    let mut gaps = Vec::new();
    let mut php = false;
    let func_cap = cap("CRG_MAX_CHANGED_FUNCS", 500);
    let doc_cap = cap("CRG_MAX_CHANGED_YAML_PATHS", 200);
    let mut funcs_cut = false;
    let mut yaml_cut = false;
    let mut json_cut = false;
    let mut tested = conn.prepare(
        "SELECT EXISTS(SELECT 1 FROM edges WHERE kind='TESTED_BY' AND source_qualified=?1)",
    )?;
    for (mut n, extra) in changes.nodes {
        let kind = text(&n, "kind");
        if kind == "YamlPath" {
            if yaml.len() < doc_cap {
                yaml.push(n)
            } else {
                yaml_cut = true
            }
            continue;
        }
        if kind == "JsonPath" {
            if json_paths.len() < doc_cap {
                json_paths.push(n)
            } else {
                json_cut = true
            }
            continue;
        }
        if !is_code(&n) || extra.get("verilog_kind").is_some() {
            continue;
        }
        if funcs.len() == func_cap {
            funcs_cut = true;
            continue;
        }
        let qn = text(&n, "qualified_name");
        if !is_test(&n, root)
            && !risk::exempt(&n, root)
            && extra.get("typescript_kind").is_none()
            && !tested.query_row([qn], |r| r.get::<_, bool>(0))?
        {
            let mut gap = json!({"name":n["name"],"qualified_name":qn,"file":n["file_path"],"line_start":n["line_start"],"line_end":n["line_end"]});
            if let Some((via, depth, test)) = risk::caller_route(conn, &n, root)? {
                gap["coverage"] = json!("indirect");
                gap["covered_via"] = json!(clean(&via));
                gap["covered_depth"] = json!(depth);
                gap["covered_by"] = json!([clean(&test)]);
            } else {
                gap["coverage"] = json!("none");
            }
            gaps.push(gap);
            php |= n["language"] == "php";
        }
        let score = risk::score(conn, &n, root)?
            + churn_term(*counts.get(text(&n, "file_path")).unwrap_or(&0));
        n["risk_score"] = json!(rounded(score.min(1.0)));
        funcs.push(n);
    }
    let mut priorities = funcs.clone();
    priorities.sort_by(|a, b| {
        b["risk_score"]
            .as_f64()
            .unwrap_or_default()
            .total_cmp(&a["risk_score"].as_f64().unwrap_or_default())
    });
    priorities.truncate(10);
    let score = priorities
        .iter()
        .chain(&changes.removed)
        .filter_map(|n| n["risk_score"].as_f64())
        .fold(0.0, f64::max);
    let affected = flows(conn, root, files)?;
    let indirect = gaps.iter().filter(|g| g["coverage"] == "indirect").count();
    let uncovered = gaps.len() - indirect;
    let mut summary = vec![
        format!("Analyzed {} changed file(s):", files.len()),
        format!("  - {} changed function(s)/class(es)", funcs.len()),
    ];
    if !yaml.is_empty() {
        summary.push(format!("  - {} changed YAML path(s)", yaml.len()));
    }
    if !json_paths.is_empty() {
        summary.push(format!("  - {} changed JSON path(s)", json_paths.len()));
    }
    if !changes.removed.is_empty() {
        summary.push(format!(
            "  - {} removed function(s)/class(es)",
            changes.removed.len()
        ));
        let used: Vec<_> = changes
            .removed
            .iter()
            .filter(|r| r["used_by_total"] != 0)
            .take(5)
            .map(|r| {
                let users = r["used_by"].as_array().unwrap();
                let total = r["used_by_total"].as_u64().unwrap_or_default() as usize;
                let names = users
                    .iter()
                    .map(|u| text(u, "qualified_name").rsplit("::").next().unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join(", ");
                let extra = if total > users.len() {
                    format!(" +{} more", total - users.len())
                } else {
                    String::new()
                };
                format!(
                    "{} (risk {:.2}, by {names}{extra})",
                    text(r, "qualified_name").rsplit("::").next().unwrap_or(""),
                    r["risk_score"].as_f64().unwrap_or_default()
                )
            })
            .collect();
        if !used.is_empty() {
            summary.push(format!("  - Removed but still used: {}", used.join("; ")));
        }
    }
    summary.push(format!("  - {} affected flow(s)", affected.len()));
    let note = if indirect > 0 {
        format!(
            " ({uncovered} with no tested caller found, {indirect} reached only through a caller)"
        )
    } else {
        String::new()
    };
    summary.push(format!("  - {} test gap(s){note}", gaps.len()));
    summary.push(format!("  - Overall risk score: {score:.2}"));
    if uncovered > 0 {
        summary.push(format!("  - Untested: {}", names(&gaps, "none")));
    }
    if indirect > 0 {
        summary.push(format!(
            "  - Reached only through a caller: {}",
            names(&gaps, "indirect")
        ));
    }
    if php {
        summary.push("  - PHP gaps are a guess: tests that reach code through the container or an HTTP call are not traced (tests_for names likely ones)".into());
    }
    for (is_cut, limit, label, var) in [
        (funcs_cut, func_cap, "functions", "CRG_MAX_CHANGED_FUNCS"),
        (
            yaml_cut,
            doc_cap,
            "YAML paths",
            "CRG_MAX_CHANGED_YAML_PATHS",
        ),
        (
            json_cut,
            doc_cap,
            "JSON paths",
            "CRG_MAX_CHANGED_YAML_PATHS",
        ),
    ] {
        if is_cut {
            summary.push(format!(
                "  - Warning: analysis capped at {limit} {label} (set {var} to adjust)"
            ));
        }
    }
    if !unavailable.is_empty() {
        summary.push(format!(
            "  - Warning: line-level diff unavailable, whole files scored ({unavailable})"
        ));
    }
    if changes.unread > 0 {
        summary.push(format!("  - Note: the old version of {} file(s) could not be read, so code deleted there is not listed as removed (CRG_MAX_DELETION_FILES sets how many files are read)",changes.unread));
    }
    if churn == "unavailable" {
        summary.push(format!("  - Degraded: change-frequency risk unavailable (git history did not answer within {}s; set CRG_CHURN_TIMEOUT to raise the budget). Risk scores exclude the churn term.",std::env::var("CRG_CHURN_TIMEOUT").unwrap_or_else(|_|"5".into())));
    }
    Ok(
        json!({"summary":summary.join("\n"),"risk_score":score,"changed_functions":funcs,"changed_yaml_paths":yaml,"changed_json_paths":json_paths,"affected_flows":affected,"test_gaps":gaps,"test_gaps_total":gaps.len(),"test_gaps_uncovered":uncovered,"test_gaps_indirect":indirect,"removed_symbols":changes.removed,"removed_symbols_total":changes.removed.len(),"review_priorities":priorities,"functions_truncated":funcs_cut,"yaml_paths_truncated":yaml_cut,"json_paths_truncated":json_cut,"diff_ranges_unavailable":unavailable,"churn_status":churn}),
    )
}

pub(in crate::cli) fn print(
    conn: &Connection,
    root: &Path,
    base: &str,
    brief: bool,
    churn: bool,
    verify: bool,
    details: Option<&str>,
) -> Result<()> {
    let files = changes::discover(root, base)?;
    let requested = base;
    let base = changes::resolve_base(root, requested)?;
    if requested == "HEAD~1" && base != requested {
        let note = if base != "HEAD" {
            "this repository has no commits yet; reviewing uncommitted and new files instead."
        } else if changes::git(root, &["rev-parse", "--is-shallow-repository"])?.1 == b"true\n" {
            "HEAD~1 is not in this shallow clone; reviewing uncommitted and new files instead. Run `git fetch --deepen=1` to review the last commit."
        } else {
            "this repository has one commit; reviewing uncommitted and new files instead."
        };
        eprintln!("Note: {note}");
    }
    if files.is_empty() {
        println!("No changes detected.");
        return Ok(());
    }
    let mut result = analysis(conn, root, &files, &base, churn)?;
    if let Some(raw) = details {
        let selected = details::parse(raw).map_err(anyhow::Error::msg)?;
        if !selected.is_empty() {
            result["details"] = details::build(conn, root, &files, &result, &selected, churn)?;
        }
    }
    let baseline = impact::file_tokens(root, &files);
    impact::savings(&mut result, conn, root, &files, "detect-changes", "", 0);
    if brief {
        render::brief(conn, root, &result)?;
        if result["changed_functions"]
            .as_array()
            .is_none_or(Vec::is_empty)
        {
            println!("Token Savings: none to show, as no functions changed.");
        } else {
            if verify {
                println!(
                    "Note: --verify requires tiktoken. Install with \x60pip install tiktoken\x60."
                );
            }
            render::panel(&result, baseline);
        }
    } else {
        println!("{}", crate::cli::wire::json_text(&result, true));
    }
    Ok(())
}

pub(super) fn run(conn: &Connection, root: &Path, args: &Value, hints: &Hints) -> Result<Value> {
    let max = positive(args, "max_results", 25, 200)?;
    let flow_cap = positive(args, "max_flows", 20, 200)?;
    let base = args["base"].as_str().unwrap_or("HEAD~1");
    let files = match changes::paths(root, args) {
        Ok(f) => f,
        Err(e) => return Ok(json!({"status":"error","error":e.to_string()})),
    };
    let base = match changes::resolve_base(root, base) {
        Ok(b) => b,
        Err(e) => return Ok(json!({"status":"error","error":e.to_string()})),
    };
    if files.is_empty() {
        return Ok(
            json!({"status":"ok","summary":"No changed files detected.","risk_score":0.0,"changed_functions":[],"changed_yaml_paths":[],"changed_json_paths":[],"affected_flows":[],"test_gaps":[],"review_priorities":[]}),
        );
    }
    let mut out = analysis(conn, root, &files, &base, true)?;
    if args["include_source"] == true {
        let mut left = 600;
        let mut src = HashMap::new();
        for func in out["changed_functions"]
            .as_array_mut()
            .into_iter()
            .flatten()
            .take(max)
        {
            if left == 0 {
                break;
            }
            let path = Path::new(text(func, "file_path"));
            if !path.is_file() || !path.canonicalize().is_ok_and(|p| p.starts_with(root)) {
                continue;
            }
            if let Ok(bytes) = std::fs::read(path) {
                let text = String::from_utf8_lossy(&bytes);
                let start = func["line_start"].as_u64().unwrap_or(1).saturating_sub(1) as usize;
                let end = (func["line_end"].as_u64().unwrap_or(0) as usize).min(start + left);
                let lines = text
                    .lines()
                    .enumerate()
                    .skip(start)
                    .take(end.saturating_sub(start))
                    .map(|(i, s)| format!("{}: {s}", i + 1))
                    .collect::<Vec<_>>();
                left -= lines.len();
                func["source"] = json!(lines.join("\n"));
                src.insert(func["id"].as_i64(), func["source"].clone());
            } else {
                func["source"] = json!("(could not read file)");
            }
        }
        for p in out["review_priorities"]
            .as_array_mut()
            .into_iter()
            .flatten()
        {
            if let Some(s) = src.get(&p["id"].as_i64()) {
                p["source"] = s.clone();
            }
        }
    }
    let detail = args["detail_level"].as_str().unwrap_or("standard");
    let len = |k: &str| out[k].as_array().map_or(0, Vec::len);
    let mut result = if detail == "minimal" {
        json!({"status":"ok","summary":out["summary"],"risk_score":out["risk_score"],"changed_file_count":files.len(),"changed_yaml_path_count":len("changed_yaml_paths"),"changed_json_path_count":len("changed_json_paths"),"test_gap_count":len("test_gaps"),"removed_symbol_count":out["removed_symbols_total"],"review_priorities":out["review_priorities"].as_array().into_iter().flatten().take(3).map(|v|&v["name"]).collect::<Vec<_>>(),"churn_status":out["churn_status"]})
    } else {
        let mut r = json!({"status":"ok"});
        r.as_object_mut()
            .unwrap()
            .extend(out.as_object().unwrap().clone());
        r["changed_files"] = json!(&files[..files.len().min(max)]);
        r["changed_file_count"] = json!(files.len());
        let mut cut = files.len() > max
            || out["json_paths_truncated"] == true
            || out["yaml_paths_truncated"] == true;
        for (k, cap) in [
            ("changed_functions", max),
            ("changed_yaml_paths", max),
            ("changed_json_paths", max),
            ("test_gaps", max),
            ("removed_symbols", max),
            ("affected_flows", flow_cap),
        ] {
            let n = len(k);
            r[format!("{k}_total")] = json!(n);
            let rows = r[k].as_array_mut().unwrap();
            cut |= rows.len() > cap;
            rows.truncate(cap);
            if k == "affected_flows" {
                for row in rows {
                    *row = subset(
                        row,
                        &[
                            "id",
                            "name",
                            "criticality",
                            "depth",
                            "node_count",
                            "file_count",
                        ],
                    );
                }
            }
        }
        if cut {
            let mut parts = Vec::new();
            for (key, label) in [
                ("changed_functions", "changed function(s)"),
                ("changed_yaml_paths", "changed YAML path(s)"),
                ("changed_json_paths", "changed JSON path(s)"),
                ("test_gaps", "test gap(s)"),
                ("affected_flows", "flow(s)"),
                ("removed_symbols", "removed symbol(s)"),
            ] {
                if len(key) == 0 && matches!(key, "changed_json_paths" | "removed_symbols") {
                    continue;
                }
                parts.push(format!(
                    "{} of {} {label}",
                    r[key].as_array().unwrap().len(),
                    len(key)
                ));
            }
            r["summary"] = json!(format!(
                "{}\n  - Response bounded: {} shown",
                text(&out, "summary"),
                parts.join(", ")
            ));
        }
        r["truncated"] = json!(cut);
        r
    };
    hints.add(&mut result, "detect_changes_tool");
    impact::savings(
        &mut result,
        conn,
        root,
        &files,
        "detect_changes_tool",
        detail,
        integer(args, "max_depth", 2)?,
    );
    Ok(result)
}

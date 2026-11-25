use super::*;
use crate::cli::query::{is_test, test_count};
use std::collections::HashSet;

fn exempt(node: &Value, root: &Path) -> bool {
    let name = text(node, "name");
    node["kind"] == "Function"
        && (matches!(
            name,
            "setUp"
                | "tearDown"
                | "setUpBeforeClass"
                | "tearDownAfterClass"
                | "setup_method"
                | "teardown_method"
                | "setUpClass"
                | "tearDownClass"
                | "__construct"
                | "__init__"
                | "__destruct"
        ) || (matches!(name, "up" | "down")
            && Path::new(text(node, "file_path"))
                .strip_prefix(root)
                .ok()
                .and_then(|p| p.parent())
                .is_some_and(|p| p.iter().any(|s| s == "migrate" || s == "migrations"))))
}

pub(super) fn gaps<'a>(radius: &'a Radius, root: &Path) -> Vec<&'a Value> {
    let tested: HashSet<_> = radius
        .edges
        .iter()
        .filter(|e| e["kind"] == "TESTED_BY")
        .map(|e| text(e, "source"))
        .collect();
    radius
        .changed
        .iter()
        .filter(|n| {
            n["kind"] == "Function"
                && !is_test(n, root)
                && !exempt(n, root)
                && !tested.contains(text(n, "qualified_name"))
        })
        .collect()
}

fn score(conn: &Connection, node: &Value, root: &Path) -> Result<f64> {
    let id = node["id"].as_i64().unwrap_or_default();
    let qn = text(node, "qualified_name");
    let flows:f64=conn.query_row("SELECT coalesce(sum(f.criticality),count(m.flow_id)*0.05) FROM flow_memberships m LEFT JOIN flows f ON f.id=m.flow_id WHERE node_id=?1",[id],|r|r.get(0))?;
    let (callers,cross):(i64,i64)=conn.query_row("SELECT count(e.id),count(DISTINCT CASE WHEN c.community_id != n.community_id THEN c.qualified_name END) FROM nodes n JOIN edges e ON e.target_qualified=n.qualified_name AND e.kind='CALLS' LEFT JOIN nodes c ON c.qualified_name=e.source_qualified WHERE n.id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let coverage = if is_test(node, root) && exempt(node, root) {
        0.0
    } else {
        0.30 - (test_count(conn, node)?.min(5) as f64 / 5.0) * 0.25
    };
    let name = qn.to_lowercase();
    let sensitive = [
        "auth",
        "login",
        "password",
        "token",
        "session",
        "crypt",
        "secret",
        "credential",
        "permission",
        "sql",
        "query",
        "execute",
        "connect",
        "socket",
        "request",
        "http",
        "sanitize",
        "validate",
        "encrypt",
        "decrypt",
        "hash",
        "sign",
        "verify",
        "admin",
        "privilege",
    ]
    .iter()
    .any(|k| name.contains(k));
    let sum = flows.min(0.25)
        + (cross as f64 * 0.05).min(0.15)
        + coverage
        + if sensitive { 0.2 } else { 0.0 }
        + (callers as f64 / 20.).min(0.1);
    Ok((sum.min(1.0) * 1e4).round_ties_even() / 1e4)
}

pub(super) fn by_file(
    conn: &Connection,
    root: &Path,
    files: &[String],
    nodes: &HashMap<String, Vec<&Value>>,
) -> Result<HashMap<String, f64>> {
    let mut risks = HashMap::new();
    let mut left = 400;
    let mut candidates = Vec::new();
    for file in files {
        let mut selected = Vec::new();
        for node in &nodes[file] {
            if matches!(text(node, "kind"), "Function" | "Test" | "Class")
                && node.get("verilog_kind").is_none()
            {
                selected.push(*node);
            }
            if selected.len() == 8 {
                break;
            }
        }
        candidates.push(selected);
        risks.insert(file.clone(), 0.0_f64);
    }
    for depth in 0..8 {
        for (file, selected) in files.iter().zip(&candidates) {
            if left == 0 {
                return Ok(risks);
            }
            if let Some(node) = selected.get(depth) {
                let value = score(conn, node, root)?;
                risks
                    .entry(file.clone())
                    .and_modify(|risk| *risk = risk.max(value));
                left -= 1;
            }
        }
    }
    Ok(risks)
}

fn tested_caller(conn: &Connection, node: &Value, root: &Path) -> Result<bool> {
    let start = text(node, "qualified_name").to_owned();
    let mut seen = HashSet::from([start.clone()]);
    let mut frontier = vec![start];
    let cap = std::env::var("CRG_CALLER_TEST_ROUTE_MAX_CALLERS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(500);
    let mut incoming=conn.prepare("SELECT DISTINCT n.* FROM edges e JOIN nodes n ON n.qualified_name=e.source_qualified WHERE e.target_qualified=?1 AND e.kind='CALLS' AND instr(e.source_qualified,'::')>0 AND json_type(e.extra,'$.unresolved_targets') IS NULL AND json_type(e.extra,'$.ambiguous_targets') IS NULL ORDER BY n.qualified_name")?;
    let mut has_test=conn.prepare("SELECT EXISTS(SELECT 1 FROM edges e JOIN nodes n ON n.qualified_name=e.target_qualified WHERE e.kind='TESTED_BY' AND e.source_qualified=?1 AND json_type(e.extra,'$.unresolved_targets') IS NULL AND json_type(e.extra,'$.ambiguous_targets') IS NULL AND (n.is_test OR n.kind='Test'))")?;
    for _ in 0..2 {
        let mut next = Vec::new();
        for qn in frontier {
            let count: i64 = conn.query_row(
                "SELECT count(*) FROM edges WHERE kind='CALLS' AND target_qualified=?1",
                [&qn],
                |r| r.get(0),
            )?;
            if count > cap {
                continue;
            }
            for caller in incoming.query_map([&qn], crate::cli::query::node)? {
                let caller = caller?;
                let name = text(&caller, "qualified_name");
                if !seen.insert(name.to_string()) {
                    continue;
                }
                if caller["kind"] == "Test"
                    || (!is_test(&caller, root)
                        && has_test.query_row([name], |r| r.get::<_, bool>(0))?)
                {
                    return Ok(true);
                }
                next.push(name.to_string());
            }
        }
        frontier = next;
    }
    Ok(false)
}

pub(super) fn guidance(
    conn: &Connection,
    radius: &Radius,
    root: &Path,
    untested: &[&Value],
) -> Result<String> {
    if radius.resolved.is_empty() {
        return Ok(
            "- None of these files are in the graph, so the blast radius is unknown.".into(),
        );
    }
    let mut lines = Vec::new();
    let names = |list: &[&Value]| {
        list.iter()
            .take(5)
            .map(|n| text(n, "name"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !untested.is_empty() {
        lines.push(format!(
            "- {} changed function(s) have no direct test: {}",
            untested.len(),
            names(untested)
        ));
        let mut indirect = Vec::new();
        let mut unreached = Vec::new();
        for n in untested {
            if tested_caller(conn, n, root)? {
                indirect.push(*n)
            } else {
                unreached.push(*n)
            }
        }
        if !indirect.is_empty() {
            lines.push(format!("  - of those, {} are reached only through a caller (a call path, not a record of execution): {}",indirect.len(),names(&indirect)));
            lines.push(format!(
                "  - {} have no tested caller found: {}",
                unreached.len(),
                names(&unreached)
            ));
        }
    }
    if radius.impacted.len() > 20 {
        lines.push(format!(
            "- Wide blast radius: {} nodes impacted. Review callers and dependents carefully.",
            radius.impacted.len()
        ));
    }
    let inheritance = radius
        .edges
        .iter()
        .filter(|e| matches!(text(e, "kind"), "INHERITS" | "IMPLEMENTS"))
        .count();
    if inheritance > 0 {
        lines.push(format!("- {inheritance} inheritance/implementation relationship(s) affected. Check for Liskov substitution violations."));
    }
    if radius.files.len() > 3 {
        lines.push(format!(
            "- Changes impact {} other files. Consider splitting into smaller PRs.",
            radius.files.len()
        ));
    }
    if lines.is_empty() {
        lines.push("- Changes appear well-contained with minimal blast radius.".into());
    }
    Ok(lines.join("\n"))
}

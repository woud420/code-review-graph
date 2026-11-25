mod confidence;
mod doc_metadata;
pub(super) use confidence::empty as empty_confidence;
mod relationships;
mod route;
mod tests_for;
use super::{store, terms};
use anyhow::{Result, ensure};
use rusqlite::{Connection, Row};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

#[derive(Default, Deserialize)]
pub struct Query {
    pub pattern: String,
    pub target: String,
    #[serde(default)]
    pub detail_level: Option<String>,
    #[serde(default)]
    pub max_results: Option<usize>,
    #[serde(default)]
    pub resolution: Option<String>,
    pub to: Option<String>,
}

pub fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| *c >= ' ' || matches!(c, '\t' | '\n'))
        .take(256)
        .collect()
}
pub fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

pub fn node(row: &Row<'_>) -> rusqlite::Result<Value> {
    let parent: Option<String> = row.get("parent_name")?;
    let mut data = json!({"id":row.get::<_,i64>("id")?, "kind":row.get::<_,String>("kind")?, "name":clean(&row.get::<_,String>("name")?),
        "qualified_name":clean(&row.get::<_,String>("qualified_name")?), "file_path":row.get::<_,String>("file_path")?,
        "line_start":row.get::<_,Option<i64>>("line_start")?, "line_end":row.get::<_,Option<i64>>("line_end")?,
        "language":row.get::<_,Option<String>>("language")?.unwrap_or_default(), "parent_name":parent.as_deref().map(clean), "is_test":row.get::<_,bool>("is_test")?});
    doc_metadata::add(&mut data, row.get("extra")?);
    Ok(data)
}

fn nodes(conn: &Connection, condition: &str, arg: &str) -> Result<Vec<Value>> {
    let mut statement = conn.prepare(&format!("SELECT * FROM nodes WHERE {condition}"))?;
    Ok(statement
        .query_map([arg], node)?
        .collect::<rusqlite::Result<_>>()?)
}

pub(super) struct Edge {
    source: String,
    target: String,
    file: String,
    line: i64,
    extra: Value,
    pub(super) wire: Value,
}
pub(super) fn edge(r: &Row<'_>) -> rusqlite::Result<Edge> {
    let kind: String = r.get("kind")?;
    let source: String = r.get("source_qualified")?;
    let target: String = r.get("target_qualified")?;
    let file: String = r.get("file_path")?;
    let line = r.get::<_, Option<i64>>("line")?.unwrap_or_default();
    let mut wire = json!({"id":r.get::<_,i64>("id")?,"kind":kind,"source":clean(&source),"target":clean(&target),"file_path":file,"line":line,
            "confidence":r.get::<_,Option<f64>>("confidence")?,"confidence_tier":r.get::<_,Option<String>>("confidence_tier")?});
    if r.get::<_, Option<String>>("target_resolution")?.as_deref() == Some("unresolved") {
        wire["target_resolution"] = json!("unresolved");
    }
    let extra: Value = r
        .get::<_, Option<String>>("extra")?
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null);
    for prefix in ["ambiguous", "unresolved"] {
        let key = format!("{prefix}_targets");
        if let Some(list) = extra[&key].as_array() {
            let names: Vec<_> = list
                .iter()
                .take(20)
                .filter_map(Value::as_str)
                .map(clean)
                .collect();
            let count = extra[format!("{prefix}_target_count")]
                .as_u64()
                .unwrap_or(list.len() as u64);
            let truncated =
                count > names.len() as u64 || extra[format!("{prefix}_targets_truncated")] == true;
            wire[&key] = json!(names);
            wire[format!("{prefix}_target_count")] = json!(count);
            wire[format!("{prefix}_targets_truncated")] = json!(truncated);
        }
    }
    Ok(Edge {
        source,
        target,
        file,
        line,
        extra,
        wire,
    })
}

fn read_edges(conn: &Connection, incoming: bool, key: &str, kind: &str) -> Result<Vec<Edge>> {
    let field = if incoming {
        "target_qualified"
    } else {
        "source_qualified"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT * FROM edges WHERE {field}=?1 AND kind=?2 ORDER BY id"
    ))?;
    let rows = stmt.query_map([key, kind], edge)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn subset(row: &Value, keys: &[&str]) -> Value {
    let mut result = json!({});
    for k in keys {
        if let Some(v) = row.get(*k) {
            result[*k] = v.clone();
        }
    }
    result
}

enum Selection {
    Found(Value, String, usize),
    Absent,
    Ambiguous(Value),
}
pub(super) fn is_test(n: &Value, root: &Path) -> bool {
    if n["is_test"] == true {
        return true;
    }
    let path = Path::new(text(n, "file_path"));
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    name.starts_with("test_")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("_test.go")
        || path.strip_prefix(root).is_ok_and(|p| {
            p.iter().any(|s| {
                ["test", "tests", "__tests__", "spec"]
                    .iter()
                    .any(|part| s == *part)
            })
        })
}

fn fuzzy(conn: &Connection, target: &str, limit: usize) -> Result<(Vec<Value>, usize)> {
    use rusqlite::params_from_iter;
    let words: Vec<_> = target.split_whitespace().collect();
    if words.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let fts = words
        .iter()
        .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ");
    let predicates =
        vec!["(lower(n.name) LIKE ? OR lower(n.qualified_name) LIKE ?)"; words.len()].join(" AND ");
    let args: Vec<_> = words
        .iter()
        .flat_map(|w| {
            [
                format!("%{}%", w.to_lowercase()),
                format!("%{}%", w.to_lowercase()),
            ]
        })
        .collect();
    for (query,params) in [("FROM nodes_fts f JOIN nodes n ON f.rowid=n.id WHERE nodes_fts MATCH ? AND n.kind!='Output'".to_string(),vec![fts]),(format!("FROM nodes n WHERE {predicates} AND n.kind!='Output'"),args)] {
        let Ok(mut stmt)=conn.prepare(&format!("SELECT n.* {query} LIMIT {limit}")) else{continue};
        let Ok(rows)=stmt.query_map(params_from_iter(&params),node) else{continue};
        let result:Vec<_>=rows.collect::<rusqlite::Result<_>>()?;
        if result.is_empty(){continue;}
        let count=if result.len()<limit{result.len()}else{conn.query_row(&format!("SELECT count(*) {query}"),params_from_iter(&params),|r|r.get::<_,i64>(0))? as usize};
        return Ok((result,count));
    }
    Ok((Vec::new(), 0))
}

fn select(
    conn: &Connection,
    root: &Path,
    target: &str,
    minimal: bool,
    cap: usize,
) -> Result<Selection> {
    let (path, symbol) = target.split_once("::").unwrap_or((target, ""));
    let mut absolute = root.join(path).to_string_lossy().into_owned();
    if !symbol.is_empty() {
        absolute.push_str("::");
        absolute.push_str(symbol);
    }
    for key in [target, &absolute] {
        if let Some(n) = nodes(conn, "qualified_name=?1", key)?.pop() {
            return Ok(Selection::Found(n, target.to_owned(), 0));
        }
    }
    let mut candidates = if target.contains('.') {
        nodes(conn, "symbol=?1 ORDER BY qualified_name", target)?
    } else {
        Vec::new()
    };
    let mut skipped = 0;
    if candidates.is_empty() {
        candidates = nodes(conn, "name=?1 ORDER BY qualified_name", target)?;
        let non_tests: Vec<_> = candidates
            .iter()
            .filter(|n| !is_test(n, root))
            .cloned()
            .collect();
        if !non_tests.is_empty() {
            skipped = candidates.len() - non_tests.len();
            candidates = non_tests;
        }
    }
    let count = if candidates.is_empty() {
        let (found, total) = fuzzy(conn, target, 20)?;
        candidates = found;
        total
    } else {
        candidates.len()
    };
    if candidates.is_empty() {
        return Ok(Selection::Absent);
    }
    if candidates.len() == 1 {
        let n = candidates.remove(0);
        let target = text(&n, "qualified_name").to_owned();
        return Ok(Selection::Found(n, target, skipped));
    }
    candidates.sort_by(|a, b| {
        let rank = |n: &Value| {
            if n["qualified_name"] == target {
                0
            } else if n["name"] == target {
                1
            } else if text(n, "qualified_name")
                .to_lowercase()
                .contains(&target.to_lowercase())
            {
                2
            } else {
                3
            }
        };
        (rank(a), text(a, "qualified_name")).cmp(&(rank(b), text(b, "qualified_name")))
    });
    candidates.truncate(20);
    let result = if minimal {
        candidates.truncate(cap);
        let names: Vec<_> = candidates
            .iter()
            .map(|n| subset(n, &["qualified_name", "kind"]))
            .collect();
        let truncated = count > names.len();
        json!({"status":"ambiguous","summary":format!("'{target}' matches {count} node(s). Re-run with a qualified_name from candidates."),
            "candidates":names,"candidate_count":count,"candidates_truncated":truncated,
            "hint":format!("Use a qualified_name from candidates as the target parameter.{}",if truncated {" detail_level='standard' lists more."} else {""})})
    } else {
        json!({"status":"ambiguous","summary":format!("'{target}' matches {count} node(s). Re-run with a qualified_name from disambiguation."),
            "candidates":candidates,"disambiguation":candidates,"candidate_count":count,"candidates_truncated":count>candidates.len(),
            "hint":"Use a qualified_name from disambiguation as the target parameter."})
    };
    Ok(Selection::Ambiguous(result))
}

struct Pair {
    key: String,
    row: Value,
    edge: Option<Value>,
    unresolved: bool,
}
fn relation(pattern: &str) -> (&'static str, bool) {
    match pattern {
        "callers_of" | "advised_by" => ("CALLS", true),
        "callees_of" | "advises" => ("CALLS", false),
        "references_to" => ("REFERENCES", true),
        "imports_of" => ("IMPORTS_FROM", false),
        "children_of" => ("CONTAINS", false),
        "triggers_of" => ("TRIGGERS", false),
        "triggered_by" => ("TRIGGERS", true),
        "publishers_of" => ("PUBLISHES", true),
        "handlers_of" | "listeners_of" => ("HANDLES", true),
        "endpoints_for" => ("HANDLES", false),
        _ => unreachable!(),
    }
}

fn pairs(
    conn: &Connection,
    pattern: &str,
    selected: &Value,
    root: &Path,
    target: &str,
    resolution: &str,
) -> Result<Vec<Pair>> {
    if pattern == "file_summary" {
        let mut rows = nodes(conn, "file_path=?1 ORDER BY line_start,id", target)?;
        if rows.is_empty() {
            rows = nodes(
                conn,
                "file_path=?1 ORDER BY line_start,id",
                &root.join(target).to_string_lossy(),
            )?;
        }
        return Ok(rows
            .into_iter()
            .map(|row| Pair {
                key: String::new(),
                row,
                edge: None,
                unresolved: false,
            })
            .collect());
    }
    let qn = text(selected, "qualified_name");
    match pattern {
        "tests_for" => return tests_for::run(conn, selected, root),
        "importers_of" => return relationships::importer(conn, selected),
        "inheritors_of" => return relationships::inherits(conn, selected),
        "consumers_of" => {
            return relationships::config(conn, selected["name"].as_str().unwrap_or(target));
        }
        _ => {}
    }
    let (kind, incoming) = relation(pattern);
    let direct = read_edges(conn, incoming, qn, kind)?;
    let mut all: Vec<_> = direct.into_iter().map(|e| (e, false)).collect();
    if matches!(pattern, "callers_of" | "references_to") && qn != text(selected, "name") {
        let name = text(selected, "name");
        let seen: HashSet<String> = all.iter().map(|(e, _)| e.source.clone()).collect();
        if pattern == "callers_of" || nodes(conn, "name=?1", name)?.len() == 1 {
            let fallback = read_edges(conn, true, name, kind)?;
            for e in fallback {
                if seen.contains(&e.source)
                    || e.extra.get("unresolved_targets").is_some()
                    || e.extra.get("ambiguous_targets").is_some()
                    || !relationships::receiver_admits(&e.extra, selected)
                {
                    continue;
                }
                if let Some(n) = nodes(conn, "qualified_name=?1", &e.source)?.pop()
                    && relationships::compatible(text(selected, "language"), text(&n, "language"))
                {
                    all.push((e, true));
                }
            }
        }
    }
    let call = matches!(pattern, "callers_of" | "callees_of" | "references_to");
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for (edge, mut unresolved) in all {
        let key = if incoming { &edge.source } else { &edge.target };
        if matches!(pattern, "advises" | "advised_by") && edge.extra["aop_resolved"] != true {
            continue;
        }
        let mut row = if pattern == "imports_of" {
            let mut row = json!({"import_target":key});
            if let Some(scope) = edge.extra.get("import_scope") {
                row["import_target_kind"] = scope.clone();
            }
            row
        } else if let Some(n) = nodes(conn, "qualified_name=?1", key)?.pop() {
            if pattern == "endpoints_for" && n["kind"] != "Endpoint" {
                continue;
            }
            n
        } else if pattern == "callees_of"
            && (!key.contains("::")
                || selected["language"] == "cpp"
                || edge.wire.get("ambiguous_targets").is_some()
                || edge.wire.get("unresolved_targets").is_some())
        {
            unresolved = true;
            let mut row = json!({"kind":"Function","name":key,"qualified_name":key});
            if seen.insert(key.clone()) {
                for prefix in ["ambiguous", "unresolved"] {
                    if let Some(targets) = edge.wire.get(format!("{prefix}_targets")) {
                        row["resolution"] = json!(prefix);
                        row["candidates"] = targets.clone();
                        row["candidate_count"] =
                            edge.wire[format!("{prefix}_target_count")].clone();
                        row["candidates_truncated"] =
                            edge.wire[format!("{prefix}_targets_truncated")].clone();
                        break;
                    }
                }
            }
            row
        } else {
            continue;
        };
        if call {
            if unresolved && resolution == "direct" || !unresolved && resolution == "unresolved" {
                continue;
            }
            if unresolved {
                row["target_resolution"] = json!("unresolved");
            }
            let mut site = json!({"line":edge.line});
            if row["file_path"] != edge.file {
                site["file"] = json!(edge.file);
            }
            row["call_site"] = site;
        }
        result.push(Pair {
            key: key.clone(),
            row,
            edge: (pattern != "children_of").then_some(edge.wire),
            unresolved,
        });
    }
    if call {
        let mut rounds: HashMap<String, usize> = HashMap::new();
        let mut numbered: Vec<_> = result
            .into_iter()
            .map(|pair| {
                let round = rounds.entry(pair.key.clone()).or_default();
                let rank = (*round, pair.unresolved);
                *round += 1;
                (rank, pair)
            })
            .collect();
        numbered.sort_by_key(|(rank, _)| *rank);
        result = numbered.into_iter().map(|(_, p)| p).collect();
    }
    Ok(result)
}

pub fn run(conn: &Connection, root: &Path, q: &Query) -> Result<Value> {
    let pattern = q.pattern.as_str();
    let raw = &q.target;
    let minimal = q.detail_level.as_deref() == Some("minimal");
    let limit = q.max_results.unwrap_or(100);
    let resolution = q.resolution.as_deref().unwrap_or("all");
    ensure!(
        limit > 0,
        "max_results must be an integer greater than or equal to 1"
    );
    ensure!(
        matches!(resolution, "direct" | "unresolved" | "all"),
        "resolution must be one of ['all', 'direct', 'unresolved'], got '{resolution}'"
    );
    let Some((_, description)) = terms::PATTERNS.iter().find(|(p, _)| *p == pattern) else {
        let available: Vec<_> = terms::PATTERNS
            .iter()
            .map(|(p, _)| format!("'{p}'"))
            .collect();
        return Ok(
            json!({"status":"error","error":format!("Unknown pattern '{pattern}'. Available: [{}]", available.join(", "))}),
        );
    };
    if pattern == "path_to" && q.to.as_deref().is_none_or(str::is_empty) {
        return Ok(
            json!({"status":"error","error":"path_to needs to: the node or file to reach."}),
        );
    }
    if pattern == "path_to" {
        return route::run(
            conn,
            root,
            q,
            q.to.as_deref().expect("validated route destination"),
            description,
            minimal,
        );
    }
    if pattern == "callers_of" && terms::BUILTINS.contains(&raw.as_str()) {
        return Ok(
            json!({"status":"ok","pattern":pattern,"target":raw,"description":description,"summary":format!("'{raw}' is a common builtin — callers_of skipped to avoid noise."),
            "result_count":0,"results_omitted":0,"results":[],"edges":[]}),
        );
    }
    let cap = if minimal { limit.min(5) } else { limit };
    let (selected, target, skipped) = if pattern == "file_summary"
        || pattern == "consumers_of" && !raw.contains("::")
    {
        (Value::Null, raw.clone(), 0)
    } else {
        match select(conn, root, raw, minimal, cap)? {
            Selection::Found(n, target, skipped) => (n, target, skipped),
            Selection::Ambiguous(data) => return Ok(data),
            Selection::Absent => {
                return Ok(
                    json!({"status":"not_found","summary":format!("No node found matching '{raw}'."),"confidence":empty_confidence(conn,root,raw,None,pattern)?}),
                );
            }
        }
    };
    let rows = pairs(conn, pattern, &selected, root, &target, resolution)?;
    let total = rows.len();
    let omitted = total.saturating_sub(cap);
    let distinct = rows.iter().map(|p| &p.key).collect::<HashSet<_>>().len();
    let uncertain = rows.iter().filter(|p| p.unresolved).count();
    let call = matches!(pattern, "callers_of" | "callees_of" | "references_to");
    let mut summary = if call {
        format!("Found {total} call site(s) across {distinct} node(s) for {pattern}('{target}')")
    } else {
        format!("Found {total} result(s) for {pattern}('{target}')")
    };
    if omitted > 0 {
        summary += &format!(" — showing {}, {omitted} omitted", total - omitted);
    }
    if skipped > 0 {
        summary += &format!(" — skipped {skipped} same-named test symbol(s)");
    }
    if pattern == "tests_for" {
        let guesses = rows
            .iter()
            .filter(|p| p.row.get("inferred_by").is_some())
            .count();
        if guesses > 0 {
            summary += &format!("; {guesses} of them guesses (see inferred_by)");
        }
    }
    let mut values = Vec::new();
    let mut edges = Vec::new();
    for pair in rows.into_iter().take(cap) {
        values.push(if minimal {
            subset(
                &pair.row,
                &[
                    "name",
                    "kind",
                    "file_path",
                    "indirect",
                    "inferred_by",
                    "call_site",
                    "target_resolution",
                ],
            )
        } else {
            pair.row
        });
        if let Some(edge) = pair.edge {
            edges.push(edge);
        }
    }
    let mut response = json!({"status":"ok","pattern":pattern,"target":target,"description":description,"summary":summary,"result_count":total,"results_omitted":omitted,"results":values});
    if !minimal {
        response["edges"] = json!(edges);
    }
    if call {
        response["distinct_nodes"] = json!(distinct);
        response["resolution_split"] = json!({"direct":total-uncertain,"unresolved":uncertain});
    }
    if total == 0 {
        response["confidence"] = json!(empty_confidence(
            conn,
            root,
            &target,
            (!selected.is_null()).then_some(&selected),
            pattern
        )?);
    } else if uncertain > 0 && resolution == "all" {
        response["confidence"] = json!(format!(
            "{uncertain} of {total} call sites match by name only and may be wrong; resolution='direct' keeps the confirmed ones"
        ));
    }
    Ok(response)
}

pub(super) fn test_count(conn: &Connection, node: &Value) -> Result<usize> {
    Ok(tests_for::explicit(conn, node)?.0.len())
}

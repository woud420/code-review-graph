mod communities;
mod cpp;
mod flows;
mod fts;
mod patterns;

use anyhow::Result;
use rusqlite::{Transaction, params};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    None,
    Minimal,
    Full,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub timing: std::collections::BTreeMap<String, f64>,
    pub bare_edges_resolved: usize,
    pub cpp_scoped_edges_resolved: usize,
    pub signatures_computed: usize,
    pub fts_indexed: usize,
    pub fts_delta: Option<(usize, usize)>,
    pub flows_detected: usize,
    pub communities_detected: usize,
}

struct Node {
    id: i64,
    name: String,
    kind: String,
    qn: String,
    file: String,
    language: String,
    parent: Option<String>,
    test: bool,
    extra: Value,
}

struct Edge {
    kind: String,
    src: String,
    dst: String,
}

fn nodes(tx: &Transaction<'_>) -> Result<Vec<Node>> {
    Ok(tx.prepare("SELECT id,name,kind,qualified_name,file_path,coalesce(language,''),parent_name,is_test,extra FROM nodes ORDER BY id")?
        .query_map([],|r| Ok(Node{id:r.get(0)?,name:r.get(1)?,kind:r.get(2)?,qn:r.get(3)?,file:r.get(4)?,language:r.get(5)?,parent:r.get(6)?,
            test:r.get::<_,Option<bool>>(7)?.unwrap_or(false),extra:serde_json::from_str(&r.get::<_,Option<String>>(8)?.unwrap_or_default()).unwrap_or(Value::Null)}))?
        .collect::<rusqlite::Result<_>>()?)
}

fn edges(tx: &Transaction<'_>) -> Result<Vec<Edge>> {
    Ok(tx
        .prepare("SELECT kind,source_qualified,target_qualified FROM edges ORDER BY kind,source_qualified,target_qualified,file_path,line")?
        .query_map([], |r| {
            Ok(Edge {
                kind: r.get(0)?,
                src: r.get(1)?,
                dst: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn resolve_bare(tx: &Transaction<'_>, nodes: &[Node]) -> Result<usize> {
    let mut names: HashMap<&str, Vec<&Node>> = HashMap::new();
    for node in nodes {
        if node.kind != "File" {
            names.entry(&node.name).or_default().push(node);
        }
    }
    let mut imports: HashMap<String, HashSet<String>> = HashMap::new();
    let mut packages: HashMap<String, HashSet<String>> = HashMap::new();
    let mut stmt = tx.prepare("SELECT file_path,target_qualified,json_extract(extra,'$.import_scope') FROM edges WHERE kind='IMPORTS_FROM'")?;
    for row in stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })? {
        let (file, target, scope) = row?;
        if scope.as_deref() == Some("package") {
            packages.entry(file).or_default().insert(target);
        } else {
            imports.entry(file).or_default().insert(target);
        }
    }
    let mut updated = 0;
    for (kind, endpoint, raw_key) in [
        ("CALLS", "target_qualified", "bare_call_target"),
        ("TESTED_BY", "source_qualified", "bare_tested_by_source"),
    ] {
        let query = format!(
            "SELECT id,{endpoint},file_path,extra FROM edges WHERE kind=? AND instr({endpoint},'::')=0 ORDER BY id"
        );
        let mut stmt = tx.prepare(&query)?;
        let candidates = stmt
            .query_map([kind], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut write = tx.prepare(&format!("UPDATE edges SET {endpoint}=?,extra=? WHERE id=?"))?;
        for (id, name, file, raw) in candidates {
            let Some(options) = names.get(name.as_str()) else {
                continue;
            };
            let mut extra: Value = serde_json::from_str(raw.as_deref().unwrap_or("{}"))?;
            if !extra.is_object()
                || [
                    "ambiguous_targets",
                    "unresolved_targets",
                    "claimed_endpoint",
                ]
                .iter()
                .any(|k| extra.get(k).is_some())
            {
                continue;
            }
            let binding = extra["receiver_binding"].as_str();
            let matches = options
                .iter()
                .filter(|n| {
                    let receiver_file = extra["receiver_module"]
                        .as_str()
                        .or_else(|| extra["receiver_class_file"].as_str());
                    let supported = n.file == file
                        || imports
                            .get(&file)
                            .is_some_and(|files| files.contains(&n.file))
                        || packages.get(&file).is_some_and(|dirs| {
                            std::path::Path::new(&n.file)
                                .parent()
                                .and_then(|p| p.to_str())
                                .is_some_and(|dir| dirs.contains(dir))
                        })
                        || receiver_file == Some(&n.file);
                    supported
                        && match binding {
                            Some("class") => {
                                n.parent.as_deref()
                                    == extra["receiver_class"]
                                        .as_str()
                                        .or_else(|| extra["receiver_type"].as_str())
                            }
                            Some("module") => receiver_file == Some(&n.file) && n.parent.is_none(),
                            Some(_) => false,
                            None => {
                                file.ends_with(".go")
                                    || extra.get("member_call").is_some()
                                    || extra.get("receiver").is_none()
                                        && extra.get("receiver_form").is_none()
                            }
                        }
                })
                .map(|n| n.qn.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            if matches.is_empty() {
                continue;
            }
            extra[raw_key] = Value::String(name.clone());
            if matches.len() == 1 {
                write.execute(params![matches.first().unwrap(), extra.to_string(), id])?;
                updated += 1;
            } else {
                extra["ambiguous_target_count"] = Value::from(matches.len());
                extra["ambiguous_targets_truncated"] = Value::from(matches.len() > 20);
                extra["ambiguous_targets"] =
                    serde_json::json!(matches.into_iter().take(20).collect::<Vec<_>>());
                write.execute(params![name, extra.to_string(), id])?;
            }
        }
    }
    Ok(updated)
}

pub fn process(tx: &Transaction<'_>, level: Level, full: bool) -> Result<Report> {
    let mut report = Report::default();
    let entities = if level == Level::None {
        Vec::new()
    } else {
        nodes(tx)?
    };
    tx.execute_batch(
        "UPDATE edges SET extra=json_set(extra, '$.claimed_endpoint',target_qualified),
        target_qualified=substr(target_qualified,instr(target_qualified,'::')+2)
        WHERE kind='REFERENCES' AND instr(target_qualified,'::')>0
        AND NOT EXISTS (SELECT 1 FROM nodes WHERE qualified_name=edges.target_qualified)",
    )?;
    if level != Level::None {
        report.bare_edges_resolved = resolve_bare(tx, &entities)?;
        report.cpp_scoped_edges_resolved = cpp::scoped_calls(tx, &entities)?;
    }
    tx.execute_batch(include_str!("certainty.sql"))?;
    if level == Level::None {
        return Ok(report);
    }
    let stage = std::time::Instant::now();
    report.signatures_computed=tx.execute("UPDATE nodes SET signature=substr(CASE kind WHEN 'Class' THEN 'class '||name WHEN 'Function' THEN 'def '||name||'('||coalesce(params,'')||')'||CASE WHEN coalesce(return_type,'')='' THEN '' ELSE ' -> '||return_type END WHEN 'Test' THEN 'def '||name||'('||coalesce(params,'')||')'||CASE WHEN coalesce(return_type,'')='' THEN '' ELSE ' -> '||return_type END ELSE name END,1,512) WHERE signature IS NULL",[])?;
    report
        .timing
        .insert("signatures_s".into(), stage.elapsed().as_secs_f64());
    let stage = std::time::Instant::now();
    report.fts_delta = fts::sync(tx, entities.len(), full)?;
    report
        .timing
        .insert("fts_s".into(), stage.elapsed().as_secs_f64());
    report.fts_indexed = entities.len();
    if level == Level::Full {
        let links = edges(tx)?;
        let stage = std::time::Instant::now();
        report.flows_detected = flows::store(tx, &entities, &links)?;
        report
            .timing
            .insert("flows_s".into(), stage.elapsed().as_secs_f64());
        let stage = std::time::Instant::now();
        report.communities_detected = communities::store(tx, &entities, &links)?;
        report
            .timing
            .insert("communities_s".into(), stage.elapsed().as_secs_f64());
        tx.execute_batch("INSERT OR REPLACE INTO metadata VALUES('postprocess_level','full');
            INSERT OR REPLACE INTO metadata VALUES('last_postprocessed_at',strftime('%Y-%m-%dT%H:%M:%S','now','localtime'))")?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO metadata VALUES('build_state','complete')",
        [],
    )?;
    Ok(report)
}

// Match round(value, 4) on the original binary float without an intermediate
// multiplication that could turn a near tie into an exact tie.
fn round_score(value: f64) -> f64 {
    format!("{value:.4}").parse().expect("finite graph score")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn bare_calls_and_test_coverage_resolve_only_with_import_evidence() -> Result<()> {
        let mut db = Connection::open_in_memory()?;
        let tx = db.transaction()?;
        tx.execute_batch(include_str!("schema.sql"))?;
        tx.execute_batch(
            "INSERT INTO nodes(kind,name,qualified_name,file_path,updated_at) VALUES
            ('Function','run','pkg.py::run','pkg.py',0),
            ('Function','run','other.py::run','other.py',0),
            ('Test','test_run','test.py::test_run','test.py',0);
            INSERT INTO edges(kind,source_qualified,target_qualified,file_path,updated_at) VALUES
            ('IMPORTS_FROM','test.py','pkg.py','test.py',0),
            ('CALLS','test.py::test_run','run','test.py',0),
            ('TESTED_BY','run','test.py::test_run','test.py',0),
            ('CALLS','external.py','run','external.py',0)",
        )?;
        assert_eq!(process(&tx, Level::Minimal, true)?.bare_edges_resolved, 2);
        let rows = tx
            .prepare(
                "SELECT kind,source_qualified,target_qualified,confidence_tier FROM edges
            WHERE kind!='IMPORTS_FROM' ORDER BY id",
            )?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(
            rows,
            [
                (
                    "CALLS".into(),
                    "test.py::test_run".into(),
                    "pkg.py::run".into(),
                    "INFERRED".into()
                ),
                (
                    "TESTED_BY".into(),
                    "pkg.py::run".into(),
                    "test.py::test_run".into(),
                    "INFERRED".into()
                ),
                (
                    "CALLS".into(),
                    "external.py".into(),
                    "run".into(),
                    "UNRESOLVED".into()
                ),
            ]
        );
        assert_eq!(process(&tx, Level::Minimal, true)?.bare_edges_resolved, 0);
        Ok(())
    }
}

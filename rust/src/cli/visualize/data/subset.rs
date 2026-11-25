use super::{LISTED, Names, communities, flows, node, payload};
use crate::cli::{
    query,
    visualize::{Options, neighbourhood},
};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

fn total(conn: &Connection, names: &Names<'_>) -> Result<Option<i64>> {
    let mut total = 0i64;
    let mut lost = Vec::new();
    for (column, source) in [("target_qualified", false), ("source_qualified", true)] {
        let sql = format!("SELECT {column},count(*) FROM edges GROUP BY {column}");
        for item in conn
            .prepare(&sql)?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        {
            let (raw, count) = item?;
            let clean = query::clean(&raw);
            if names.resolve(&clean, &clean).is_some() {
                if raw != clean {
                    return Ok(None);
                }
                if !source {
                    total += count
                }
            } else if source {
                lost.push(raw)
            }
        }
    }
    if !lost.is_empty() {
        let sql = "SELECT target_qualified,count(*) FROM edges
                   WHERE source_qualified IN (SELECT value FROM json_each(?))
                   GROUP BY target_qualified";
        let mut statement = conn.prepare(sql)?;
        let counts = statement.query_map([serde_json::to_string(&lost)?], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in counts {
            let (target, count) = row?;
            let name = query::clean(&target);
            if names.resolve(&name, &name).is_some() {
                total -= count;
            }
        }
    }
    Ok(Some(total))
}

fn surrounding(
    conn: &Connection,
    names: &Names<'_>,
    wanted: &HashSet<String>,
) -> Result<Vec<Value>> {
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let raw = names.stored_ends(wanted);
    let mut touching = Vec::new();
    let sql = "SELECT * FROM edges
               WHERE source_qualified IN (SELECT value FROM json_each(?1))
                  OR target_qualified IN (SELECT value FROM json_each(?1))
               ORDER BY id";
    let mut statement = conn.prepare(sql)?;
    for row in statement.query_map([serde_json::to_string(&raw)?], query::edge)? {
        if let Some(edge) = names.edge(row?.wire)
            && (wanted.contains(query::text(&edge, "source"))
                || wanted.contains(query::text(&edge, "target")))
        {
            touching.push(edge);
        }
    }
    Ok(touching)
}

pub(in crate::cli::visualize) fn seeded(
    conn: &Connection,
    opt: &Options,
    patterns: &[String],
) -> Result<Value> {
    if opt.path_query().is_none()
        && let Some(part) = subset(conn, opt, patterns)?
    {
        return Ok(part);
    }
    neighbourhood::extract(&super::read(conn)?, opt, patterns)
}

fn subset(conn: &Connection, opt: &Options, patterns: &[String]) -> Result<Option<Value>> {
    let mut identities = Vec::new();
    let sql = format!(
        "SELECT id,qualified_name,name,kind,file_path FROM nodes WHERE {LISTED} ORDER BY file_path,id"
    );
    for r in conn.prepare(&sql)?.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    })? {
        let (id, qn, name, kind, file) = r?;
        if qn != query::clean(&qn) || file.contains('\\') {
            return Ok(None);
        }
        identities.push(json!({"id":id,"qualified_name":qn,"name":query::clean(&name),"kind":kind,"file_path":file}));
    }
    let names = Names::new(&identities);
    let Some(edge_count) = total(conn, &names)? else {
        return Ok(None);
    };
    let flows = flows(conn)?;
    let mut reached: HashSet<_> = neighbourhood::seed_names(&identities, &flows, opt, patterns)?
        .into_iter()
        .map(str::to_owned)
        .collect();
    let mut frontier = reached.clone();
    let mut edges = BTreeMap::<i64, Value>::new();
    for hop in 0..=opt.depth() {
        if reached.len() > 5000.max(identities.len() / 4) {
            return Ok(None);
        }
        let mut next = HashSet::new();
        for e in surrounding(conn, &names, &frontier)? {
            if e["kind"] != "CONTAINS" && hop < opt.depth() {
                let (a, b) = (query::text(&e, "source"), query::text(&e, "target"));
                for (src, dst) in [(a, b), (b, a)] {
                    if frontier.contains(src) && !reached.contains(dst) {
                        next.insert(dst.to_owned());
                    }
                }
            }
            edges.insert(e["id"].as_i64().unwrap(), e);
        }
        if next.is_empty() {
            break;
        }
        reached.extend(next.iter().cloned());
        frontier = next;
    }
    let mut parents = HashSet::new();
    let mut parented = HashSet::new();
    for e in edges.values().filter(|e| e["kind"] == "CONTAINS") {
        let (source, target) = (query::text(e, "source"), query::text(e, "target"));
        if reached.contains(target) && parented.insert(target) && !reached.contains(source) {
            parents.insert(source.to_owned());
        }
    }
    for e in surrounding(conn, &names, &parents)? {
        edges.insert(e["id"].as_i64().unwrap(), e);
    }
    reached.extend(parents);
    let sql = "SELECT * FROM nodes WHERE qualified_name IN
               (SELECT value FROM json_each(?)) ORDER BY file_path,id";
    let nodes = conn
        .prepare(sql)?
        .query_map([serde_json::to_string(&reached)?], node)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let members = communities(&nodes);
    let data = payload(conn, nodes, edges.into_values().collect(), flows, members)?;
    let mut selected = neighbourhood::extract(&data, opt, patterns)?;
    selected["neighbourhood"]["total_nodes"] = json!(identities.len());
    selected["neighbourhood"]["total_edges"] = json!(edge_count);
    Ok(Some(selected))
}

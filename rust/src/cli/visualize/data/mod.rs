use crate::cli::{query, reports, store};
use anyhow::Result;
use rusqlite::{Connection, Row};
mod subset;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
pub(super) use subset::seeded;

struct Names<'a> {
    qualified: HashSet<&'a str>,
    short: HashMap<String, Vec<&'a str>>,
}

impl<'a> Names<'a> {
    fn new(nodes: &'a [Value]) -> Self {
        let mut names = Self {
            qualified: HashSet::new(),
            short: HashMap::new(),
        };
        for n in nodes {
            let qn = query::text(n, "qualified_name");
            let name = query::text(n, "name");
            let fp = query::text(n, "file_path");
            names.qualified.insert(qn);
            let mut add = |key: String| names.short.entry(key).or_default().push(qn);
            add(name.into());
            if qn.contains("::") {
                add(qn.rsplit('/').next().unwrap_or(qn).into());
            }
            if !fp.is_empty() {
                let module = fp.replace('/', ".").replace(".py", "");
                if n["kind"] == "File" {
                    add(module);
                    let normalized = fp.replace('\\', "/");
                    add(normalized.clone());
                    for (i, _) in normalized.match_indices('/') {
                        if i + 1 < normalized.len() {
                            add(normalized[i + 1..].into());
                        }
                    }
                } else {
                    add(format!("{module}.{name}"));
                }
            }
        }
        names
    }

    fn resolve(&self, target: &str, source: &str) -> Option<&'a str> {
        if let Some(&qn) = self.qualified.get(target) {
            return Some(qn);
        }
        let choices = self.short.get(target)?;
        if choices.len() == 1 {
            return Some(choices[0]);
        }
        let file = source.split("::").next().unwrap_or(source);
        for prefix in [file, file.rsplit_once('/').map_or("", |(d, _)| d)] {
            let mut matches = choices.iter().copied().filter(|n| n.starts_with(prefix));
            if let Some(first) = matches.next()
                && matches.next().is_none()
            {
                return Some(first);
            }
        }
        choices.first().copied()
    }
}

const LISTED: &str = "file_path IN (SELECT file_path FROM nodes WHERE extra IS NULL OR json_valid(extra)=0 OR coalesce(json_extract(extra,'$.virtual'),0)!=1 UNION SELECT file_path FROM edges)";

fn node(r: &Row<'_>) -> rusqlite::Result<Value> {
    let mut node = query::node(r)?;
    node["params"] = json!(r.get::<_, Option<String>>("params")?);
    node["return_type"] = json!(r.get::<_, Option<String>>("return_type")?);
    node["community_id"] = json!(r.get::<_, Option<i64>>("community_id")?);
    Ok(node)
}

impl Names<'_> {
    fn stored_ends<'a>(&'a self, wanted: &'a HashSet<String>) -> HashSet<&'a str> {
        let mut raw: HashSet<_> = wanted.iter().map(String::as_str).collect();
        for (alias, targets) in &self.short {
            if targets.iter().any(|name| wanted.contains(*name)) {
                raw.insert(alias);
            }
        }
        raw
    }

    fn edge(&self, mut edge: Value) -> Option<Value> {
        let raw = query::text(&edge, "source");
        let source = self.resolve(raw, raw)?;
        let target = self.resolve(query::text(&edge, "target"), raw)?;
        edge["source"] = source.into();
        edge["target"] = target.into();
        Some(edge)
    }
}

fn flows(conn: &Connection) -> Result<Vec<Value>> {
    Ok(conn
        .prepare("SELECT * FROM flows ORDER BY criticality DESC LIMIT 100")?
        .query_map([], reports::flow_row)?
        .collect::<rusqlite::Result<_>>()?)
}

fn payload(
    conn: &Connection,
    nodes: Vec<Value>,
    edges: Vec<Value>,
    flows: Vec<Value>,
    group_members: HashMap<i64, Vec<String>>,
) -> Result<Value> {
    let counts = store::Counts::read(conn)?;
    let stats = json!({"total_nodes":counts.nodes(),"total_edges":counts.edges(),"nodes_by_kind":counts.node_kinds,"edges_by_kind":counts.edge_kinds,"languages":counts.languages,"files_count":counts.files(),"last_updated":counts.updated});
    let mut groups = reports::group_rows(conn, "size DESC", 0)?;
    for group in &mut groups {
        group["members"] = json!(
            group_members
                .get(&group["id"].as_i64().unwrap())
                .cloned()
                .unwrap_or_default()
        );
    }
    Ok(json!({"nodes":nodes,"edges":edges,"stats":stats,"flows":flows,"communities":groups}))
}

fn communities(nodes: &[Value]) -> HashMap<i64, Vec<String>> {
    let mut rows = nodes
        .iter()
        .filter_map(|node| {
            Some((
                node["id"].as_i64()?,
                node["community_id"].as_i64()?,
                query::text(node, "qualified_name"),
            ))
        })
        .collect::<Vec<_>>();
    rows.sort_unstable_by_key(|r| r.0);
    let mut groups = HashMap::<_, Vec<_>>::new();
    for (_, id, name) in rows {
        groups.entry(id).or_default().push(name.to_owned());
    }
    groups
}

pub(super) fn read(conn: &Connection) -> Result<Value> {
    let nodes = conn
        .prepare(&format!(
            "SELECT * FROM nodes WHERE {LISTED} ORDER BY file_path,id"
        ))?
        .query_map([], node)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let names = Names::new(&nodes);
    let mut edges = Vec::new();
    for row in conn
        .prepare("SELECT * FROM edges ORDER BY id")?
        .query_map([], query::edge)?
    {
        if let Some(edge) = names.edge(row?.wire) {
            edges.push(edge);
        }
    }
    // Full exports include every community member, including virtual-only files.
    let mut members = HashMap::<_, Vec<_>>::new();
    let sql = "SELECT community_id,qualified_name FROM nodes
               WHERE community_id IS NOT NULL ORDER BY id";
    let mut statement = conn.prepare(sql)?;
    for row in statement.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let (id, name) = row?;
        members.entry(id).or_default().push(query::clean(&name));
    }
    payload(conn, nodes, edges, flows(conn)?, members)
}

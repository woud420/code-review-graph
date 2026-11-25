use crate::cli::{query, reports, store};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

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

pub(super) fn read(conn: &Connection) -> Result<Value> {
    let nodes = conn.prepare("SELECT * FROM nodes WHERE file_path IN (SELECT file_path FROM nodes WHERE extra IS NULL OR json_valid(extra)=0 OR coalesce(json_extract(extra,'$.virtual'),0)!=1 UNION SELECT file_path FROM edges) ORDER BY file_path,id")?
        .query_map([],|r|{let mut n=query::node(r)?;
            n["params"]=json!(r.get::<_,Option<String>>("params")?);
            n["return_type"]=json!(r.get::<_,Option<String>>("return_type")?);
            n["community_id"]=json!(r.get::<_,Option<i64>>("community_id")?);
            Ok(n)})?.collect::<rusqlite::Result<Vec<_>>>()?;
    let names = Names::new(&nodes);
    let mut edges = Vec::new();
    for row in conn
        .prepare("SELECT * FROM edges ORDER BY id")?
        .query_map([], query::edge)?
    {
        let mut edge = row?.wire;
        let raw = query::text(&edge, "source");
        if let (Some(source), Some(target)) = (
            names.resolve(raw, raw),
            names.resolve(query::text(&edge, "target"), raw),
        ) {
            edge["source"] = source.into();
            edge["target"] = target.into();
            edges.push(edge);
        }
    }
    let counts = store::Counts::read(conn)?;
    let stats = json!({"total_nodes":counts.nodes(),"total_edges":counts.edges(),"nodes_by_kind":counts.node_kinds,"edges_by_kind":counts.edge_kinds,"languages":counts.languages,"files_count":counts.files(),"last_updated":counts.updated});
    let flows = conn
        .prepare("SELECT * FROM flows ORDER BY criticality DESC LIMIT 100")?
        .query_map([], reports::flow_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut groups = reports::group_rows(conn, "size DESC", 0)?;
    let mut members = conn.prepare("SELECT qualified_name FROM nodes WHERE community_id=?")?;
    for group in &mut groups {
        group["members"] = json!(
            members
                .query_map([group["id"].as_i64()], |r| Ok(query::clean(
                    &r.get::<_, String>(0)?
                )))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        );
    }
    Ok(json!({"nodes":nodes,"edges":edges,"stats":stats,"flows":flows,"communities":groups}))
}

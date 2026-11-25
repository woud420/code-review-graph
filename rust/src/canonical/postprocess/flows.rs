use super::{Edge, Node, patterns::*};
use crate::model::test_file;
use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction, params};
use std::collections::{HashMap, HashSet, VecDeque};

pub fn conventional_entry(name: &str, language: &str, extra: &serde_json::Value) -> bool {
    let decorated = match extra.get("decorators") {
        Some(serde_json::Value::String(name)) => DECORATOR.is_match(name),
        Some(serde_json::Value::Array(names)) => names
            .iter()
            .filter_map(|n| n.as_str())
            .any(|n| DECORATOR.is_match(n)),
        _ => false,
    };
    ENTRY.is_match(name) || (language == "php" && PHP_ENTRY.is_match(name)) || decorated
}

struct Adjacency {
    out: Vec<Vec<usize>>,
    external: Vec<usize>,
    called: Vec<bool>,
    called_outside_tests: Vec<bool>,
    tested: Vec<bool>,
}

impl Adjacency {
    fn new(nodes: &[Node], edges: &[Edge], repo: Option<&str>) -> Self {
        let by_name: HashMap<_, _> = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.qn.as_str(), i))
            .collect();
        let n = nodes.len();
        let mut result = Self {
            out: vec![Vec::new(); n],
            external: vec![0; n],
            called: vec![false; n],
            called_outside_tests: vec![false; n],
            tested: vec![false; n],
        };
        for edge in edges {
            let source = by_name.get(edge.src.as_str()).copied();
            if edge.kind == "TESTED_BY" {
                if let Some(s) = source {
                    result.tested[s] = true;
                }
            } else if edge.kind == "CALLS" {
                if let Some(&target) = by_name.get(edge.dst.as_str()) {
                    if let Some(s) = source {
                        result.out[s].push(target);
                        if nodes[s].kind != "File" {
                            result.called[target] = true;
                            if !nodes[s].test
                                && nodes[s].kind != "Test"
                                && !test_file(&nodes[s].file, repo)
                            {
                                result.called_outside_tests[target] = true;
                            }
                        }
                    } else {
                        result.called[target] = true;
                    }
                } else if let Some(s) = source {
                    result.external[s] += 1;
                }
            }
        }
        result
    }

    fn is_entry_point(&self, index: usize, node: &Node, repo: Option<&str>) -> bool {
        if !matches!(node.kind.as_str(), "Function" | "Test")
            || node.test
            || test_file(&node.file, repo)
            || node.extra.get("verilog_kind").is_some()
        {
            return false;
        }
        let called = if matches!(node.language.as_str(), "ruby" | "java") {
            self.called_outside_tests[index]
        } else {
            self.called[index]
        };
        !called || conventional_entry(&node.name, &node.language, &node.extra)
    }

    fn trace(&self, start: usize) -> (Vec<usize>, usize) {
        let mut path = vec![start];
        let mut seen = HashSet::from([start]);
        let mut queue = VecDeque::from([(start, 0usize)]);
        let mut max_depth = 0;
        while let Some((node, depth)) = queue.pop_front() {
            max_depth = max_depth.max(depth);
            if depth >= 15 {
                continue;
            }
            for &next in &self.out[node] {
                if seen.insert(next) {
                    path.push(next);
                    queue.push_back((next, depth + 1));
                }
            }
        }
        (path, max_depth)
    }

    fn score(&self, path: &[usize], nodes: &[Node], files: usize, depth: usize) -> f64 {
        let external: usize = path.iter().map(|&i| self.external[i]).sum();
        let security = path
            .iter()
            .filter(|&&i| {
                let qn = nodes[i].qn.to_lowercase();
                let name = nodes[i].name.to_lowercase();
                SECURITY.iter().any(|w| qn.contains(w) || name.contains(w))
            })
            .count();
        let tested = path.iter().filter(|&&i| self.tested[i]).count();
        let size = path.len() as f64;
        let value = 0.30 * ((files - 1) as f64 / 4.0).min(1.0)
            + 0.20 * (external as f64 / 5.0).min(1.0)
            + 0.25 * security as f64 / size
            + 0.15 * (1.0 - tested as f64 / size)
            + 0.10 * (depth as f64 / 10.0).min(1.0);
        super::round_score(value)
    }
}

pub(super) fn store(tx: &Transaction<'_>, nodes: &[Node], edges: &[Edge]) -> Result<usize> {
    let repo: Option<String> = tx
        .query_row(
            "SELECT value FROM metadata WHERE key='repo_root'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let adj = Adjacency::new(nodes, edges, repo.as_deref());
    let mut flows = Vec::new();
    for (start, node) in nodes.iter().enumerate() {
        if !adj.is_entry_point(start, node, repo.as_deref()) {
            continue;
        }
        let (path, depth) = adj.trace(start);
        if path.len() < 2 {
            continue;
        }
        let files = path
            .iter()
            .map(|&i| &nodes[i].file)
            .collect::<HashSet<_>>()
            .len();
        let score = adj.score(&path, nodes, files, depth);
        flows.push((score, start, depth, files, path));
    }
    flows.sort_by(|a, b| b.0.total_cmp(&a.0));
    tx.execute_batch("DELETE FROM flow_memberships; DELETE FROM flows; DELETE FROM metadata WHERE key='csharp_flows_dirty';")?;
    let mut insert=tx.prepare("INSERT INTO flows(name,entry_point_id,depth,node_count,file_count,criticality,path_json) VALUES(?,?,?,?,?,?,?)")?;
    let mut membership =
        tx.prepare("INSERT INTO flow_memberships(flow_id,node_id,position) VALUES(?,?,?)")?;
    for (score, start, depth, files, path) in &flows {
        let name: String = nodes[*start]
            .name
            .chars()
            .filter(|c| *c == '\t' || *c == '\n' || *c >= ' ')
            .take(256)
            .collect();
        let ids: Vec<i64> = path.iter().map(|&i| nodes[i].id).collect();
        insert.execute(params![
            name,
            nodes[*start].id,
            *depth as i64,
            ids.len() as i64,
            *files as i64,
            score,
            serde_json::to_string(&ids)?
        ])?;
        let flow_id = tx.last_insert_rowid();
        for (position, id) in ids.iter().enumerate() {
            membership.execute(params![flow_id, id, position as i64])?;
        }
    }
    Ok(flows.len())
}

pub struct RemovedFlowNode {
    pub name: String,
    pub qualified_name: String,
    pub file: String,
    pub callers: Vec<String>,
    pub callees: Vec<String>,
}

/// Reinsert deleted call targets in memory so review reports can measure the flows they broke.
pub fn removed_flow_scores(
    conn: &rusqlite::Connection,
    removed: &[RemovedFlowNode],
) -> Result<HashMap<String, f64>> {
    if removed.is_empty() {
        return Ok(HashMap::new());
    }
    let mut nodes = super::nodes(conn)?;
    let mut edges = super::edges(conn)?;
    let repo: Option<String> = conn
        .query_row(
            "SELECT value FROM metadata WHERE key='repo_root'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let original = Adjacency::new(&nodes, &edges, repo.as_deref());
    let callers: HashSet<_> = removed
        .iter()
        .flat_map(|r| r.callers.iter().map(String::as_str))
        .collect();
    let mut entry_ids = nodes
        .iter()
        .enumerate()
        .filter(|(i, n)| {
            callers.contains(n.qn.as_str()) && original.is_entry_point(*i, n, repo.as_deref())
        })
        .map(|(_, n)| n.id)
        .collect::<HashSet<_>>();
    let names = serde_json::to_string(&callers)?;
    let mut query=conn.prepare("SELECT DISTINCT f.entry_point_id FROM flows f JOIN flow_memberships m ON f.id=m.flow_id JOIN nodes n ON n.id=m.node_id WHERE n.qualified_name IN(SELECT value FROM json_each(?1)) AND f.entry_point_id IS NOT NULL")?;
    for id in query.query_map([names], |r| r.get::<_, i64>(0))? {
        entry_ids.insert(id?);
    }
    let start = nodes.len();
    for (i, r) in removed.iter().enumerate() {
        nodes.push(Node {
            id: -(i as i64 + 1),
            name: r.name.clone(),
            qn: r.qualified_name.clone(),
            file: r.file.clone(),
            kind: "Function".into(),
            language: String::new(),
            parent: None,
            test: false,
            extra: serde_json::Value::Null,
        });
    }
    let existing: HashSet<_> = nodes.iter().map(|n| n.qn.as_str()).collect();
    for r in removed {
        for caller in &r.callers {
            edges.push(Edge {
                kind: "CALLS".into(),
                src: caller.clone(),
                dst: r.qualified_name.clone(),
            });
        }
        for target in &r.callees {
            let dst = if target.contains("::") || existing.contains(target.as_str()) {
                target.clone()
            } else {
                format!("{}::{target}", r.file)
            };
            edges.push(Edge {
                kind: "CALLS".into(),
                src: r.qualified_name.clone(),
                dst,
            });
        }
    }
    let adj = Adjacency::new(&nodes, &edges, repo.as_deref());
    let mut scores = HashMap::new();
    for (i, n) in nodes[..start].iter().enumerate() {
        if !entry_ids.contains(&n.id) {
            continue;
        }
        let (path, depth) = adj.trace(i);
        let files = path
            .iter()
            .map(|&i| &nodes[i].file)
            .collect::<HashSet<_>>()
            .len();
        let score = adj.score(&path, &nodes, files, depth);
        for i in path.into_iter().filter(|&i| i >= start) {
            *scores.entry(nodes[i].qn.clone()).or_insert(0.0) += score;
        }
    }
    Ok(scores)
}

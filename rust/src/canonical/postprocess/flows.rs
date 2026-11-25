use super::{Edge, Node, patterns::*};
use crate::model::test_file;
use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction, params};
use std::collections::{HashMap, HashSet, VecDeque};

fn conventional(node: &Node) -> bool {
    let decorated = match node.extra.get("decorators") {
        Some(serde_json::Value::String(name)) => DECORATOR.is_match(name),
        Some(serde_json::Value::Array(names)) => names
            .iter()
            .filter_map(|n| n.as_str())
            .any(|n| DECORATOR.is_match(n)),
        _ => false,
    };
    ENTRY.is_match(&node.name)
        || (node.language == "php" && PHP_ENTRY.is_match(&node.name))
        || decorated
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
        !called || conventional(node)
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

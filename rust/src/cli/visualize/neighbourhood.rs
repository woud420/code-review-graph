use super::Options;
use crate::cli::query::{clean, text};
use anyhow::{Result, bail, ensure};
use globset::GlobBuilder;
use serde_json::{Value, json};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

type Neighbours<'a> = BTreeMap<&'a str, BTreeSet<&'a str>>;
const ROUTE: &[&str] = &["CALLS", "IMPORTS_FROM", "INHERITS"];

fn normal(path: &str) -> String {
    path.replace('\\', "/")
        .trim()
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_owned()
}

fn symbol<'a>(nodes: &'a [Value], q: &str) -> Result<Vec<&'a str>> {
    let names = nodes
        .iter()
        .map(|n| text(n, "qualified_name"))
        .collect::<BTreeSet<_>>();
    let normalized = normal(q);
    for query in [q, &normalized] {
        if let Some(&name) = names.get(query) {
            return Ok(vec![name]);
        }
    }
    let tail = format!("::{q}");
    let suffix = format!("/{normalized}");
    let matching = names
        .iter()
        .copied()
        .filter(|n| n.ends_with(&tail) || n.ends_with(&suffix))
        .collect::<Vec<_>>();
    if !matching.is_empty() {
        return Ok(matching);
    }
    let mut bare = nodes
        .iter()
        .filter(|n| text(n, "name") == q)
        .map(|n| text(n, "qualified_name"))
        .collect::<Vec<_>>();
    ensure!(
        !bare.is_empty(),
        "no node in the graph matches symbol {}; pass a qualified name such as 'pkg/mod.py::func'",
        repr(q)
    );
    bare.sort_unstable();
    Ok(bare)
}

fn repr(s: &str) -> String {
    crate::cli::wire::value_repr(&json!(clean(s)))
}

fn files<'a>(nodes: &'a [Value], queries: &[String]) -> Result<Vec<&'a str>> {
    let mut matches = BTreeSet::new();
    for q in queries {
        let pattern = normal(q);
        if pattern.is_empty() {
            continue;
        }
        let suffix = format!("/{pattern}");
        let glob = GlobBuilder::new(&pattern)
            .backslash_escape(false)
            .literal_separator(false)
            .build()
            .ok()
            .map(|g| g.compile_matcher());
        for node in nodes {
            let path = normal(text(node, "file_path"));
            if path == pattern
                || path.ends_with(&suffix)
                || glob.as_ref().is_some_and(|g| g.is_match(&path))
            {
                matches.insert(text(node, "qualified_name"));
            }
        }
    }
    ensure!(
        !matches.is_empty(),
        "no indexed file matches: {}",
        queries
            .iter()
            .take(5)
            .map(|s| clean(s))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(matches.into_iter().collect())
}

fn flow<'a>(flows: &[Value], nodes: &'a [Value], q: &str) -> Result<Vec<&'a str>> {
    let lower = q.trim().to_lowercase();
    let selected = flows
        .iter()
        .find(|f| {
            f["id"].as_i64().is_some_and(|id| id.to_string() == q)
                || text(f, "name").to_lowercase() == lower
        })
        .or_else(|| {
            flows
                .iter()
                .find(|f| !lower.is_empty() && text(f, "name").to_lowercase().contains(&lower))
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no execution flow matches {}; run 'code-review-graph postprocess' to detect flows",
                repr(q)
            )
        })?;
    let names: HashMap<_, _> = nodes
        .iter()
        .map(|n| (n["id"].as_i64(), text(n, "qualified_name")))
        .collect();
    let matches = selected["path"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| names.get(&id.as_i64()).copied())
        .collect::<Vec<_>>();
    ensure!(
        !matches.is_empty(),
        "flow {} has no nodes present in the exported graph",
        repr(text(selected, "name"))
    );
    Ok(matches)
}

fn adjacent(edges: &[Value], route: bool, directed: bool) -> Neighbours<'_> {
    let mut map = Neighbours::new();
    for e in edges {
        let (k, src, dst) = (text(e, "kind"), text(e, "source"), text(e, "target"));
        if src.is_empty()
            || dst.is_empty()
            || (route && !ROUTE.contains(&k))
            || (!route && k == "CONTAINS")
        {
            continue;
        }
        map.entry(src).or_default().insert(dst);
        if !directed {
            map.entry(dst).or_default().insert(src);
        }
    }
    map
}

fn shortest<'a>(adj: &Neighbours<'a>, from: &'a str, to: &'a str) -> Vec<&'a str> {
    let mut queue = VecDeque::from([from]);
    let mut parent = HashMap::from([(from, None)]);
    let mut visits = 0;
    while let Some(node) = queue.pop_front() {
        visits += 1;
        if visits > 2_000_000 {
            break;
        }
        if node == to {
            let mut path = vec![node];
            let mut current = node;
            while let Some(p) = parent[current] {
                path.push(p);
                current = p;
            }
            path.reverse();
            return path;
        }
        for &next in adj.get(node).into_iter().flatten() {
            if let std::collections::hash_map::Entry::Vacant(slot) = parent.entry(next) {
                slot.insert(Some(node));
                queue.push_back(next);
            }
        }
    }
    Vec::new()
}

struct Seeds<'a> {
    nodes: Vec<&'a str>,
    kind: &'static str,
    path: Vec<&'a str>,
    directed: bool,
    error: String,
}
impl<'a> Seeds<'a> {
    fn resolve(
        nodes: &'a [Value],
        edges: &'a [Value],
        flows: &[Value],
        opt: &Options,
        patterns: &[String],
    ) -> Result<Self> {
        let mut result = Self {
            nodes: Vec::new(),
            kind: "symbol",
            path: Vec::new(),
            directed: true,
            error: String::new(),
        };
        if let Some((start, end)) = opt.path_query() {
            let (from, to) = (symbol(nodes, start)?[0], symbol(nodes, end)?[0]);
            result.kind = "path";
            result.path = shortest(&adjacent(edges, true, true), from, to);
            if result.path.is_empty() {
                result.directed = false;
                result.path = shortest(&adjacent(edges, true, false), from, to);
            }
            if result.path.is_empty() {
                result.error = format!(
                    "no path from {} to {} through {}",
                    clean(from),
                    clean(to),
                    ROUTE.join("/")
                );
                result.nodes.extend([from, to]);
            } else {
                result.nodes.extend(&result.path);
            }
        }
        for q in opt.symbols() {
            if result.kind == "path" {
                result.kind = "mixed";
            }
            result.nodes.extend(symbol(nodes, q)?);
        }
        if !patterns.is_empty() {
            result.kind = if result.kind == "symbol" {
                "file"
            } else {
                "mixed"
            };
            result.nodes.extend(files(nodes, patterns)?);
        }
        if let Some(q) = opt.seed_flow.as_deref().filter(|s| !s.is_empty()) {
            result.kind = if result.kind == "symbol" {
                "flow"
            } else {
                "mixed"
            };
            result.nodes.extend(flow(flows, nodes, q)?);
        }
        let mut seen = HashSet::new();
        result.nodes.retain(|n| seen.insert(*n));
        Ok(result)
    }
}

pub(super) fn seed_names<'a>(
    nodes: &'a [Value],
    flows: &[Value],
    options: &Options,
    patterns: &[String],
) -> Result<Vec<&'a str>> {
    Ok(Seeds::resolve(nodes, &[], flows, options, patterns)?.nodes)
}

fn distances<'a>(
    seeds: &[&'a str],
    adj: &Neighbours<'a>,
    depth: usize,
) -> BTreeMap<&'a str, usize> {
    let mut hops = seeds.iter().map(|n| (*n, 0)).collect::<BTreeMap<_, _>>();
    let mut queue = VecDeque::from(seeds.to_vec());
    while let Some(node) = queue.pop_front() {
        let d = hops[node];
        if d >= depth {
            continue;
        }
        for &next in adj.get(node).into_iter().flatten() {
            if let std::collections::btree_map::Entry::Vacant(slot) = hops.entry(next) {
                slot.insert(d + 1);
                queue.push_back(next);
            }
        }
    }
    hops
}

fn budget<'a>(
    edges: &'a [Value],
    hops: BTreeMap<&'a str, usize>,
    pinned: &[&'a str],
    limit: usize,
) -> (indexmap::IndexMap<&'a str, usize>, bool) {
    let mut degree = HashMap::<&str, usize>::new();
    let mut parents = HashMap::new();
    for e in edges {
        let (from, to) = (text(e, "source"), text(e, "target"));
        *degree.entry(from).or_default() += 1;
        *degree.entry(to).or_default() += 1;
        if text(e, "kind") == "CONTAINS" {
            parents.entry(to).or_insert(from);
        }
    }
    let mut selected = indexmap::IndexMap::new();
    for &n in pinned {
        selected.insert(n, hops[n]);
    }
    let mut order = hops.into_iter().collect::<Vec<_>>();
    order.sort_unstable_by_key(|&(n, d)| (d, Reverse(*degree.get(n).unwrap_or(&0)), n));
    let mut trimmed = false;
    for (n, d) in order {
        let already = selected.contains_key(n);
        if let Some(prev) = selected.get_mut(n) {
            *prev = (*prev).min(d);
        } else if selected.len() < limit {
            selected.insert(n, d);
        } else {
            trimmed = true;
            continue;
        }
        if let Some(&p) = parents.get(n)
            && !selected.contains_key(p)
        {
            if selected.len() < limit {
                selected.insert(p, d);
            } else if already {
                trimmed = true;
            }
        }
    }
    (selected, trimmed)
}

pub(super) fn extract(data: &Value, opt: &Options, patterns: &[String]) -> Result<Value> {
    let nodes = data["nodes"].as_array().unwrap();
    let edges = data["edges"].as_array().unwrap();
    let flows = data["flows"].as_array().unwrap();
    let seeds = Seeds::resolve(nodes, edges, flows, opt, patterns)?;
    let limit = opt.limit();
    if seeds.path.len() > limit {
        bail!(
            "max-nodes {limit} is below the {} nodes on the path from {} to {}; raise it to at least {}",
            seeds.path.len(),
            clean(seeds.path[0]),
            clean(seeds.path[seeds.path.len() - 1]),
            seeds.path.len()
        );
    }
    let depth = opt.depth();
    let hops = distances(&seeds.nodes, &adjacent(edges, false, false), depth);
    let (selection, truncated) = budget(edges, hops, &seeds.path, limit);
    let mut seen = HashSet::new();
    let kept_nodes = nodes
        .iter()
        .filter(|n| {
            let name = text(n, "qualified_name");
            selection.contains_key(name) && seen.insert(name)
        })
        .collect::<Vec<_>>();
    let ids = kept_nodes
        .iter()
        .filter_map(|n| n["id"].as_i64())
        .collect::<HashSet<_>>();
    let kept_edges = edges
        .iter()
        .filter(|e| {
            selection.contains_key(text(e, "source")) && selection.contains_key(text(e, "target"))
        })
        .collect::<Vec<_>>();
    let kept_flows = flows
        .iter()
        .filter(|f| {
            f["path"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|n| n.as_i64().is_some_and(|id| ids.contains(&id)))
        })
        .collect::<Vec<_>>();
    let groups = data["communities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|g| {
            let members = g["members"]
                .as_array()?
                .iter()
                .filter(|m| m.as_str().is_some_and(|s| selection.contains_key(s)))
                .collect::<Vec<_>>();
            if members.is_empty() {
                return None;
            }
            let mut group = g.clone();
            group["members"] = json!(members);
            Some(group)
        })
        .collect::<Vec<_>>();
    let mut frontier = selection
        .iter()
        .filter_map(|(n, d)| (*d >= depth).then_some(n))
        .collect::<Vec<_>>();
    frontier.sort_unstable();
    let kept_seeds = seeds
        .nodes
        .iter()
        .filter(|n| selection.contains_key(**n))
        .collect::<Vec<_>>();
    let mut query = opt
        .symbols()
        .iter()
        .chain(patterns)
        .map(|s| clean(s))
        .collect::<Vec<_>>();
    if query.is_empty()
        && let Some(q) = &opt.seed_flow
    {
        query.push(clean(q));
    }
    let hops = selection
        .iter()
        .map(|(n, d)| (n.to_string(), json!(d)))
        .collect::<serde_json::Map<_, _>>();
    Ok(
        json!({"nodes":kept_nodes,"edges":kept_edges,"stats":data["stats"],"flows":kept_flows,"communities":groups,
        "mode":"neighbourhood","neighbourhood":{"seeds":kept_seeds,"seeds_requested":seeds.nodes.len(),"seeds_dropped":seeds.nodes.len()-kept_seeds.len(),
        "seed_kind":seeds.kind,"seed_query":query,"depth":depth,"render_depth":opt.render_depth(),"hops":hops,
        "frontier":frontier,"path":seeds.path,"path_directed":seeds.directed,"path_edge_kinds":ROUTE,"path_error":seeds.error,
        "structural_edge_kinds":["CONTAINS"],"truncated":truncated,"max_nodes":limit,"total_nodes":nodes.len(),"total_edges":edges.len()}}),
    )
}

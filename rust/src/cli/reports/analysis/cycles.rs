use super::*;
use std::collections::BTreeMap;

type Links = BTreeMap<(String, String), i64>;
type Adj = HashMap<String, Vec<String>>;
fn adjacency(links: &Links, reverse: bool) -> Adj {
    let mut out: Adj = HashMap::new();
    for (a, b) in links.keys() {
        let (a, b) = if reverse { (b, a) } else { (a, b) };
        out.entry(a.clone()).or_default().push(b.clone());
    }
    for list in out.values_mut() {
        list.sort();
    }
    out
}
fn components(links: &Links) -> Vec<Vec<String>> {
    let forward = adjacency(links, false);
    let backward = adjacency(links, true);
    let mut finish = Vec::new();
    let mut seen = HashSet::new();
    for start in links.keys().flat_map(|(a, b)| [a, b]) {
        if seen.contains(start) {
            continue;
        }
        let mut stack = vec![(start.clone(), false)];
        while let Some((n, leave)) = stack.pop() {
            if leave {
                finish.push(n);
                continue;
            }
            if !seen.insert(n.clone()) {
                continue;
            }
            stack.push((n.clone(), true));
            if let Some(next) = forward.get(&n) {
                stack.extend(next.iter().rev().map(|s| (s.clone(), false)));
            }
        }
    }
    seen.clear();
    let mut groups = Vec::new();
    for start in finish.iter().rev() {
        if seen.contains(start) {
            continue;
        }
        let mut group = Vec::new();
        let mut stack = vec![start.clone()];
        while let Some(n) = stack.pop() {
            if !seen.insert(n.clone()) {
                continue;
            }
            if let Some(next) = backward.get(&n) {
                stack.extend(next.iter().cloned());
            }
            group.push(n);
        }
        if group.len() > 1 {
            group.sort();
            groups.push(group);
        }
    }
    groups.sort_by(|a, b| {
        (std::cmp::Reverse(a.len()), &a[0]).cmp(&(std::cmp::Reverse(b.len()), &b[0]))
    });
    groups
}
fn shortest(adj: &Adj, start: &str) -> Vec<String> {
    let mut prev = HashMap::from([(start, String::new())]);
    let mut queue = VecDeque::from([start]);
    while let Some(now) = queue.pop_front() {
        for next in adj.get(now).into_iter().flatten() {
            if next == start {
                let mut path = vec![now.to_owned()];
                while path.last().is_some_and(|s| s != start) {
                    path.push(prev[path.last().expect("path has one node").as_str()].clone());
                }
                path.reverse();
                return path;
            }
            if !prev.contains_key(next.as_str()) {
                prev.insert(next, now.to_owned());
                queue.push_back(next);
            }
        }
    }
    vec![start.to_owned()]
}

pub(super) fn find(conn: &Connection, root: &Path) -> Result<Vec<Value>> {
    let mut all = Links::new();
    let mut load = Links::new();
    let mut stmt=conn.prepare("SELECT e.source_qualified,e.target_qualified,e.line,e.extra FROM edges e JOIN nodes a ON a.qualified_name=e.source_qualified AND a.kind='File' JOIN nodes b ON b.qualified_name=e.target_qualified AND b.kind='File' WHERE e.kind='IMPORTS_FROM' AND e.source_qualified!=e.target_qualified")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let extra = row
            .get::<_, Option<String>>(3)?
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or(Value::Null);
        if matches!(
            extra["import_context"].as_str(),
            Some("type_only" | "parent_package")
        ) {
            continue;
        }
        let pair = (row.get::<_, String>(0)?, row.get::<_, String>(1)?);
        let line = row.get::<_, Option<i64>>(2)?.unwrap_or(0);
        all.entry(pair.clone())
            .and_modify(|n| *n = (*n).min(line))
            .or_insert(line);
        if extra["import_context"] != "deferred" {
            load.entry(pair)
                .and_modify(|n| *n = (*n).min(line))
                .or_insert(line);
        }
    }
    let groups = components(&all);
    if groups.is_empty() {
        return Ok(Vec::new());
    }
    let load_groups = components(&load);
    let tight = adjacency(&load, false);
    let broad = adjacency(&all, false);
    let mut result = Vec::new();
    let prefix = format!("{}/", root.display());
    let short = |p: &str| clean(p.strip_prefix(&prefix).unwrap_or(p));
    for group in groups {
        let load_group = load_groups
            .iter()
            .find(|g| g.iter().all(|n| group.binary_search(n).is_ok()));
        let route = if let Some(local) = load_group {
            shortest(&tight, &local[0])
        } else {
            shortest(&broad, &group[0])
        };
        let mut hops = Vec::new();
        for i in 0..route.len().min(10) {
            let pair = (route[i].clone(), route[(i + 1) % route.len()].clone());
            hops.push(json!({"file":short(&pair.0),"line":load.get(&pair).unwrap_or(&all[&pair]),"imports":short(&pair.1)}));
        }
        let name = route
            .iter()
            .chain(std::iter::once(&route[0]))
            .map(|s| short(s))
            .collect::<Vec<_>>()
            .join(" -> ");
        result.push(json!({"name":clean(&name),"size":group.len(),"load_time":load_group.is_some(),"files":group.iter().take(10).map(|s|short(s)).collect::<Vec<_>>(),"loop_length":route.len(),"cycle":hops}));
    }
    result.sort_by(|a, b| {
        let sort = |v: &Value| {
            (
                v["load_time"] != true,
                std::cmp::Reverse(v["size"].as_u64()),
                v["name"].as_str().unwrap_or_default().to_owned(),
            )
        };
        sort(a).cmp(&sort(b))
    });
    Ok(result)
}

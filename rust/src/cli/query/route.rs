use super::*;
use std::collections::VecDeque;

const CERTAINTY: [&str; 3] = ["direct", "unique_name", "shared_name"];
const VISITS: usize = 200_000;
#[derive(Clone)]
struct Hop {
    from: String,
    to: String,
    rank: usize,
    edge: Value,
}

fn next(conn: &Connection, source: &str, include_guesses: bool) -> Result<Vec<Hop>> {
    let Some(source_node) = nodes(conn, "qualified_name=?1", source)?.pop() else {
        return Ok(Vec::new());
    };
    let mut hops = Vec::new();
    for kind in [
        "CALLS",
        "REFERENCES",
        "INHERITS",
        "IMPLEMENTS",
        "IMPORTS_FROM",
        "CONTAINS",
    ] {
        for e in read_edges(conn, false, source, kind)? {
            if kind == "IMPORTS_FROM" && e.extra["import_context"] == "type_only" {
                continue;
            }
            if !nodes(conn, "qualified_name=?1", &e.target)?.is_empty() {
                hops.push(Hop {
                    from: source.to_owned(),
                    to: e.target,
                    rank: 0,
                    edge: e.wire,
                });
                continue;
            }
            if !include_guesses || matches!(kind, "IMPORTS_FROM" | "CONTAINS") {
                continue;
            }
            let mut candidates = Vec::new();
            let mut supplied = false;
            for prefix in ["ambiguous", "unresolved"] {
                if let Some(list) = e.extra[format!("{prefix}_targets")].as_array() {
                    supplied = true;
                    if list.len() <= 10
                        && e.extra[format!("{prefix}_target_count")]
                            .as_u64()
                            .unwrap_or(0)
                            <= 10
                    {
                        for name in list.iter().filter_map(Value::as_str) {
                            candidates.extend(nodes(
                                conn,
                                "qualified_name=?1 AND is_test=0",
                                name,
                            )?);
                        }
                    }
                    break;
                }
            }
            if !supplied
                && !e.target.contains("::")
                && !terms::BUILTINS.contains(&e.target.as_str())
            {
                candidates = nodes(conn, "name=?1 AND kind!='File' AND is_test=0", &e.target)?
                    .into_iter()
                    .filter(|n| {
                        relationships::compatible(
                            text(&source_node, "language"),
                            text(n, "language"),
                        ) && relationships::receiver_admits(&e.extra, n)
                    })
                    .collect();
            }
            if candidates.len() > 10 {
                continue;
            }
            let rank = if !supplied && candidates.len() == 1 {
                1
            } else {
                2
            };
            for node in candidates {
                hops.push(Hop {
                    from: source.to_owned(),
                    to: text(&node, "qualified_name").to_owned(),
                    rank,
                    edge: e.wire.clone(),
                });
            }
        }
    }
    hops.sort_by(|a, b| {
        (a.rank, &a.to, a.edge["line"].as_i64()).cmp(&(b.rank, &b.to, b.edge["line"].as_i64()))
    });
    Ok(hops)
}

enum Route {
    Found(Vec<Hop>),
    Incomplete,
    Absent,
}
fn route(conn: &Connection, start: &str, finish: &str) -> Result<Route> {
    let mut cache = HashMap::new();
    for tier in 0..3 {
        let mut queue = VecDeque::from([start.to_owned()]);
        let mut previous: HashMap<String, Option<Hop>> = HashMap::from([(start.to_owned(), None)]);
        for _ in 0..VISITS {
            let Some(current) = queue.pop_front() else {
                break;
            };
            let key = (current.clone(), tier > 0);
            if !cache.contains_key(&key) {
                cache.insert(key.clone(), next(conn, &current, tier > 0)?);
            }
            for hop in &cache[&key] {
                if hop.rank > tier || previous.contains_key(&hop.to) {
                    continue;
                }
                previous.insert(hop.to.clone(), Some(hop.clone()));
                if hop.to == finish {
                    let mut path = Vec::new();
                    let mut current = finish;
                    while let Some(Some(h)) = previous.get(current) {
                        path.push(h.clone());
                        current = &h.from;
                    }
                    path.reverse();
                    return Ok(Route::Found(path));
                }
                queue.push_back(hop.to.clone());
            }
        }
        if !queue.is_empty() {
            return Ok(Route::Incomplete);
        }
    }
    Ok(Route::Absent)
}

pub(super) fn run(
    conn: &Connection,
    root: &Path,
    q: &Query,
    finish: &str,
    description: &str,
    minimal: bool,
) -> Result<Value> {
    let mut ends = Vec::new();
    for (which, input) in [("target", q.target.as_str()), ("to", finish)] {
        match select(conn, root, input, minimal, 5)? {
            Selection::Found(n, _, _) => ends.push(n),
            Selection::Absent => {
                return Ok(
                    json!({"status":"not_found","endpoint":which,"summary":format!("No node found matching {which} '{input}'."),"confidence":empty_confidence(conn,root,input,None,"path_to")?}),
                );
            }
            Selection::Ambiguous(v) => {
                let count = v["candidate_count"].as_u64().unwrap_or(0);
                let mut response = json!({"status":"ambiguous","endpoint":which,"summary":format!("{which} '{input}' matches {count} node(s). Re-run with a qualified_name from candidates."),"candidates":v["candidates"],"candidate_count":count,"candidates_truncated":v["candidates_truncated"],"hint":format!("Use a qualified_name from candidates as {which}.")});
                if !minimal {
                    response["disambiguation"] = v["candidates"].clone();
                }
                return Ok(response);
            }
        }
    }
    let from = text(&ends[0], "qualified_name");
    let to = text(&ends[1], "qualified_name");
    let prefix = format!("{}/", root.display());
    let relative = |s: &str| clean(s.strip_prefix(&prefix).unwrap_or(s));
    let mut out = json!({"status":"ok","pattern":"path_to","target":from,"to":to,"description":description,"result_count":0,"results_omitted":0,"results":[]});
    if from == to {
        out["summary"] = json!("target and to are the same node, so there are no hops.");
        return Ok(out);
    }
    match route(conn, from, to)? {
        Route::Incomplete => {
            out["search_incomplete"] = json!(true);
            out["summary"] = json!(format!(
                "The search stopped after {VISITS} nodes without reaching '{}', so a route may still exist. Try nearer ends.",
                relative(to)
            ));
        }
        Route::Absent => {
            let mut msg = format!(
                "No route from '{}' to '{}' through calls, references, inheritance, imports or containment. Callbacks and dynamic dispatch are not in the graph, so this does not prove there is none.",
                relative(from),
                relative(to)
            );
            if matches!(route(conn, to, from)?, Route::Found(_)) {
                out["reverse_route_exists"] = json!(true);
                msg.push_str(" There is a route the other way: swap target and to.");
            }
            out["summary"] = json!(msg);
            out["confidence"] = json!(empty_confidence(
                conn,
                root,
                &q.target,
                Some(&ends[0]),
                "path_to"
            )?);
        }
        Route::Found(hops) => {
            let mut weakest = &hops[0];
            for hop in &hops[1..] {
                if hop.rank > weakest.rank
                    || (hop.rank == weakest.rank
                        && hop.edge["confidence"].as_f64() < weakest.edge["confidence"].as_f64())
                {
                    weakest = hop;
                }
            }
            let render = |hop: &Hop| -> Result<Value> {
                let edge = &hop.edge;
                let mut row = if minimal {
                    json!({"from":relative(&hop.from),"to":relative(&hop.to),"kind":edge["kind"],"line":edge["line"],"certainty":CERTAINTY[hop.rank]})
                } else {
                    json!({"from":clean(&hop.from),"to":clean(&hop.to),"kind":edge["kind"],"file_path":edge["file_path"],"line":edge["line"],"certainty":CERTAINTY[hop.rank]})
                };
                if minimal {
                    if let Some(source) = nodes(conn, "qualified_name=?1", &hop.from)?.pop()
                        && source["file_path"] != edge["file_path"]
                    {
                        row["file"] = json!(relative(text(edge, "file_path")));
                    }
                } else if let Some(score) = edge["confidence"].as_f64().filter(|f| *f < 1.) {
                    row["confidence"] = json!(format!("{score:.2}").parse::<f64>()?);
                }
                Ok(row)
            };
            let results = hops
                .iter()
                .take(30)
                .map(render)
                .collect::<Result<Vec<_>>>()?;
            out["result_count"] = json!(hops.len());
            out["results_omitted"] = json!(hops.len() - results.len());
            out["results"] = json!(results);
            out["certainty"] = json!(CERTAINTY[weakest.rank]);
            out["weakest_hop"] = render(weakest)?;
            out["summary"] = json!(format!(
                "'{}' reaches '{}' in {} hop(s); weakest hop: {}.",
                relative(from),
                relative(to),
                hops.len(),
                CERTAINTY[weakest.rank]
            ));
        }
    }
    Ok(out)
}

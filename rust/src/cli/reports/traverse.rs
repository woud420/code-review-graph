use super::*;
use crate::cli::{search, wire::value_repr};
use std::collections::{HashMap, VecDeque};

pub(super) fn run(conn: &Connection, root: &Path, args: &Value) -> Result<Value> {
    let query = text(args, "query");
    let mode = args["mode"].as_str().unwrap_or("bfs");
    let budget = positive(args, "token_budget", 2000, 10000)?;
    let depth = integer(args, "depth", 3)?.clamp(1, 6) as usize;
    let options = search::Options {
        query: query.to_string(),
        ..Default::default()
    };
    let (_, ranked) = search::ranked_with_ids(conn, root, &options, 1)?;
    let Some((id, _)) = ranked.first() else {
        return Ok(json!({"error":format!("No node matching '{query}'"),"nodes":[]}));
    };
    let start: String =
        conn.query_row("SELECT qualified_name FROM nodes WHERE id=?1", [id], |r| {
            r.get(0)
        })?;
    let mut visited = HashMap::new();
    let mut queue = VecDeque::from([(start.clone(), 0)]);
    let mut entries = Vec::new();
    let mut tokens = 0;
    let mut node = conn.prepare("SELECT name,kind,file_path FROM nodes WHERE qualified_name=?1")?;
    let mut outward =
        conn.prepare("SELECT target_qualified FROM edges WHERE source_qualified=?1 ORDER BY id")?;
    let mut inward =
        conn.prepare("SELECT source_qualified FROM edges WHERE target_qualified=?1 ORDER BY id")?;
    while let Some((qn, level)) = if mode == "bfs" {
        queue.pop_front()
    } else {
        queue.pop_back()
    } {
        if level > depth || visited.get(&qn).is_some_and(|&d| d <= level) {
            continue;
        }
        let first = visited.insert(qn.clone(), level).is_none();
        let Some(row)=node.query_row([&qn],|r|Ok(json!({"name":clean(&r.get::<_,String>(0)?),"qualified_name":qn,"kind":r.get::<_,String>(1)?,"file":r.get::<_,String>(2)?,"depth":level}))).optional()?else{continue};
        if first {
            tokens += value_repr(&row).chars().count() / 4;
            if tokens > budget {
                break;
            }
            entries.push(row);
        }
        if level < depth {
            for stmt in [&mut outward, &mut inward] {
                for name in stmt.query_map([&qn], |r| r.get::<_, String>(0))? {
                    let name = name?;
                    if visited.get(&name).is_none_or(|&d| d > level + 1) {
                        queue.push_back((name, level + 1));
                    }
                }
            }
        }
    }
    Ok(
        json!({"start_node":clean(&start),"mode":mode,"max_depth":depth,"token_budget":budget,"nodes_visited":entries.len(),"traversal":entries,"truncated":tokens>budget,"next_tool_suggestions":["query_graph_tool callers_of -- focused relationship query","get_impact_radius_tool -- blast radius analysis"]}),
    )
}

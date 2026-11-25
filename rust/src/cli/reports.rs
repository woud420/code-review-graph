use super::query::{clean, node, subset, text};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    path::Path,
    sync::{Mutex, OnceLock},
};

fn integer(args: &Value, key: &str, default: i64) -> Result<i64> {
    match args.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_i64()
            .ok_or_else(|| anyhow::anyhow!("{key} must be an integer")),
    }
}
fn positive(args: &Value, key: &str, default: i64, cap: usize) -> Result<usize> {
    let i = integer(args, key, default)?;
    ensure!(
        i >= 1,
        "{key} must be an integer greater than or equal to 1"
    );
    Ok((i as usize).min(cap))
}
fn shown(n: usize, total: usize) -> String {
    if n < total {
        format!(", showing {n} of {total}")
    } else {
        String::new()
    }
}

pub fn large(conn: &Connection, root: &Path, args: &Value) -> Result<Value> {
    let lines = integer(args, "min_lines", 50)?;
    let minimal = args["detail_level"] == "minimal";
    let limit = positive(args, "limit", 50, if minimal { 25 } else { 100 })?;
    let kind = text(args, "kind");
    let path = text(args, "file_path_pattern");
    let filter = "FROM nodes WHERE line_start IS NOT NULL AND line_end IS NOT NULL AND line_end-line_start+1>=?1 AND extra NOT LIKE '%\"verilog_kind\"%' AND (?2='' OR kind=?2) AND (?3='' OR file_path LIKE '%'||?3||'%')";
    let total: i64 = conn.query_row(
        &format!("SELECT count(*) {filter}"),
        params![lines, kind, path],
        |r| r.get(0),
    )?;
    let total = total as usize;
    let mut stmt = conn.prepare(&format!(
        "SELECT * {filter} ORDER BY (line_end-line_start+1) DESC LIMIT ?4"
    ))?;
    let mut values = stmt
        .query_map(params![lines, kind, path, limit as i64], node)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut summary = format!("Found {total} node(s) with >= {lines} lines");
    if !kind.is_empty() {
        summary += &format!(" (kind={kind})");
    }
    if !path.is_empty() {
        summary += &format!(" matching '{path}'");
    }
    summary += &format!("{}:", shown(values.len(), total));
    for (index, n) in values.iter_mut().enumerate() {
        let f = Path::new(text(n, "file_path"));
        let rel = f
            .strip_prefix(root)
            .unwrap_or(f)
            .to_string_lossy()
            .into_owned();
        let start = n["line_start"].as_i64().unwrap_or(0);
        let end = n["line_end"].as_i64().unwrap_or(0);
        let size = if start == 0 || end == 0 {
            0
        } else {
            end - start + 1
        };
        n["line_count"] = json!(size);
        n["relative_path"] = json!(rel);
        if index < 10 {
            summary += &format!(
                "\n  {size:>4} lines | {:>8} | {} ({rel}:{start})",
                text(n, "kind"),
                text(n, "name")
            );
        }
        if minimal {
            *n = subset(
                n,
                &["name", "kind", "relative_path", "line_start", "line_count"],
            );
        }
    }
    if values.len() > 10 {
        summary += &format!("\n  ... and {} more", values.len() - 10);
    }
    Ok(
        json!({"status":"ok","summary":summary,"total_found":total,"total":total,"result_count":total,
        "results_omitted":total-values.len(),"truncated":total>values.len(),"min_lines":lines,"results":values}),
    )
}

#[derive(Default)]
pub struct Hints {
    used: Mutex<VecDeque<String>>,
}
impl Hints {
    fn add(&self, result: &mut Value, name: &str) {
        static WORKFLOW: OnceLock<Value> = OnceLock::new();
        let workflow = WORKFLOW.get_or_init(|| {
            serde_json::from_str(include_str!("workflow.json")).expect("bundled workflow")
        });
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        if used.len() >= 100 {
            used.pop_front();
        }
        used.push_back(name.into());
        let steps: Vec<_> = workflow[name]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|hint| !used.iter().any(|k| k == text(hint, "tool")))
            .take(3)
            .collect();
        result["_hints"] = json!({"next_steps":steps,"related":[],"warnings":[]});
    }
}

pub fn flows(
    conn: &Connection,
    root: &Path,
    args: &Value,
    single: bool,
    hints: &Hints,
) -> Result<Value> {
    let sort = match text(args, "sort_by") {
        "name" => "name ASC",
        "depth" => "depth DESC",
        "file_count" => "file_count DESC",
        "node_count" => "node_count DESC",
        _ => "criticality DESC",
    };
    let kind = text(args, "kind");
    let mut stmt=conn.prepare(&format!("SELECT f.* FROM flows f WHERE (?1='' OR EXISTS(SELECT 1 FROM nodes n WHERE n.id=f.entry_point_id AND n.kind=?1)) ORDER BY {sort}"))?;
    let mut flows=stmt.query_map([kind], |r|{
        let raw:String=r.get("path_json")?;
        Ok(json!({"id":r.get::<_,i64>("id")?,"name":clean(&r.get::<_,String>("name")?),"entry_point_id":r.get::<_,Option<i64>>("entry_point_id")?,
            "depth":r.get::<_,i64>("depth")?,"node_count":r.get::<_,i64>("node_count")?,"file_count":r.get::<_,i64>("file_count")?,
            "criticality":r.get::<_,f64>("criticality")?,"path":serde_json::from_str::<Value>(&raw).unwrap_or_default(),
            "created_at":r.get::<_,Option<String>>("created_at")?,"updated_at":r.get::<_,Option<String>>("updated_at")?}))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    if !single {
        let total = flows.len();
        flows.truncate(positive(args, "limit", 50, 200)?);
        if args["detail_level"] == "minimal" {
            for f in &mut flows {
                *f = subset(f, &["name", "criticality", "node_count"]);
            }
        }
        let mut value = json!({"status":"ok","summary":format!("Found {total} execution flow(s){}",shown(flows.len(),total)),"flows":flows,"total":total,"truncated":flows.len()<total});
        hints.add(&mut value, "list_flows_tool");
        return Ok(value);
    }
    let steps_max = positive(args, "max_steps", 50, 200)?;
    let mut budget = positive(args, "max_source_lines", 400, 2000)?;
    let lookup = if let Some(id) = args["flow_id"].as_i64() {
        flows.into_iter().find(|f| f["id"] == id)
    } else {
        args["flow_name"].as_str().and_then(|name| {
            flows.into_iter().take(500).find(|f| {
                text(f, "name")
                    .to_lowercase()
                    .contains(&name.to_lowercase())
            })
        })
    };
    let Some(f) = lookup else {
        return Ok(
            json!({"status":"not_found","summary":"No flow found matching the given criteria."}),
        );
    };
    let mut steps = Vec::new();
    let mut stmt = conn.prepare("SELECT * FROM nodes WHERE id=?1")?;
    for id in f["path"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_i64)
    {
        if let Some(n) = stmt.query_row([id], node).optional()? {
            steps.push(json!({"node_id":n["id"],"name":n["name"],"kind":n["kind"],"file":n["file_path"],"line_start":n["line_start"],"line_end":n["line_end"],"qualified_name":n["qualified_name"]}));
        }
    }
    let total = steps.len();
    steps.truncate(steps_max);
    let mut no_source = false;
    if args["include_source"] == true {
        for step in &mut steps {
            if budget == 0 {
                no_source = true;
                break;
            }
            let path = root.join(text(step, "file"));
            if let Ok(real) = path.canonicalize()
                && real.starts_with(root)
                && real.is_file()
            {
                if let Ok(data) = fs::read(real) {
                    let content = String::from_utf8_lossy(&data);
                    let start = step["line_start"].as_u64().unwrap_or(1).saturating_sub(1) as usize;
                    let end = step["line_end"].as_u64().unwrap_or(u64::MAX) as usize;
                    let lines: Vec<_> = content
                        .lines()
                        .enumerate()
                        .take(end)
                        .skip(start)
                        .take(budget)
                        .map(|(idx, s)| format!("{}: {s}", idx + 1))
                        .collect();
                    budget -= lines.len();
                    step["source"] = json!(lines.join("\n"));
                } else {
                    step["source"] = json!("(could not read file)");
                }
            }
        }
    }
    let mut flow = subset(
        &f,
        &[
            "id",
            "name",
            "entry_point_id",
            "depth",
            "node_count",
            "file_count",
            "criticality",
            "path",
        ],
    );
    flow["steps"] = json!(steps);
    flow["created_at"] = f["created_at"].clone();
    flow["updated_at"] = f["updated_at"].clone();
    flow["total_steps"] = json!(total);
    flow["truncated"] = json!(total > steps.len() || no_source);
    if no_source {
        flow["source_truncated"] = json!(true);
    }
    let summary = format!(
        "Flow '{}': {} nodes, depth {}, criticality {:.4}{}",
        text(&f, "name"),
        f["node_count"],
        f["depth"],
        f["criticality"].as_f64().unwrap_or_default(),
        shown(steps.len(), total)
    );
    let mut value = json!({"status":"ok","summary":summary,"flow":flow});
    hints.add(&mut value, "get_flow_tool");
    Ok(value)
}

pub fn communities(conn: &Connection, args: &Value, single: bool, hints: &Hints) -> Result<Value> {
    let sort = match text(args, "sort_by") {
        "cohesion" => "cohesion DESC",
        "name" => "name ASC",
        _ => "size DESC",
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT * FROM communities WHERE size>=?1 ORDER BY {sort}"
    ))?;
    let mut groups=stmt.query_map([integer(args,"min_size",0)?], |r| {
        Ok(json!({"id":r.get::<_,i64>("id")?,"name":clean(&r.get::<_,String>("name")?),"level":r.get::<_,i64>("level")?,
            "cohesion":r.get::<_,f64>("cohesion")?,"size":r.get::<_,i64>("size")?,"dominant_language":r.get::<_,Option<String>>("dominant_language")?.unwrap_or_default(),
            "description":clean(&r.get::<_,Option<String>>("description")?.unwrap_or_default())}))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    let member_cap = positive(args, "max_members", if single { 25 } else { 10 }, 25)?;
    let total = groups.len();
    if single {
        let group = if let Some(id) = args["community_id"].as_i64() {
            groups.into_iter().find(|g| g["id"] == id)
        } else {
            args["community_name"].as_str().and_then(|name| {
                groups.into_iter().find(|g| {
                    text(g, "name")
                        .to_lowercase()
                        .contains(&name.to_lowercase())
                })
            })
        };
        let Some(group) = group else {
            return Ok(
                json!({"status":"not_found","summary":"No community found matching the given criteria."}),
            );
        };
        groups = vec![group];
    } else {
        groups.truncate(positive(args, "max_results", 50, 200)?);
    }
    let mut stmt = conn.prepare("SELECT * FROM nodes WHERE community_id=?1")?;
    for g in &mut groups {
        if !single && args["detail_level"] == "minimal" {
            *g = subset(g, &["name", "size", "cohesion"]);
            continue;
        }
        let mut members = stmt
            .query_map([g["id"].as_i64()], node)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let member_count = members.len();
        members.truncate(member_cap);
        g["members"] = json!(
            members
                .iter()
                .map(|n| &n["qualified_name"])
                .collect::<Vec<_>>()
        );
        if single && args["include_members"] == true {
            g["member_details"] = json!(members);
        }
        if member_count > member_cap {
            g["members_total"] = json!(member_count);
            g["members_truncated"] = json!(true);
            if g.get("member_details").is_some() {
                g["member_details_total"] = json!(member_count);
            }
        }
    }
    let mut value = if single {
        let g = &groups[0];
        json!({"status":"ok","summary":format!("Community '{}': {} nodes, cohesion {:.4}",text(g,"name"),g["size"],g["cohesion"].as_f64().unwrap_or_default()),"community":g})
    } else {
        json!({"status":"ok","summary":format!("Found {total} communities{}",shown(groups.len(),total)),"communities":groups,"total":total,"truncated":groups.len()<total})
    };
    hints.add(
        &mut value,
        if single {
            "get_community_tool"
        } else {
            "list_communities_tool"
        },
    );
    Ok(value)
}

pub const NAMES: &[&str] = &[
    "find_large_functions_tool",
    "list_flows_tool",
    "get_flow_tool",
    "list_communities_tool",
    "get_community_tool",
];
pub fn run(
    conn: &Connection,
    root: &Path,
    name: &str,
    args: &Value,
    hints: &Hints,
) -> Result<Value> {
    match name {
        "find_large_functions_tool" => large(conn, root, args),
        "list_flows_tool" | "get_flow_tool" => {
            flows(conn, root, args, name == "get_flow_tool", hints)
        }
        "list_communities_tool" | "get_community_tool" => {
            communities(conn, args, name == "get_community_tool", hints)
        }
        _ => anyhow::bail!("Unknown graph report '{name}'"),
    }
}

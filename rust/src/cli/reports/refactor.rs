pub(super) mod apply;
mod dead;
pub(in crate::cli) use dead::find as dead_code;
mod rename;
use super::*;

fn suggestions(conn: &Connection, root: &Path) -> Result<Vec<Value>> {
    let mut results = Vec::new();
    for n in dead::find(conn, root, "", "")? {
        results.push(json!({"type":"remove","description":format!("Remove unused {} '{}'",text(&n,"kind").to_lowercase(),text(&n,"name")),"symbols":[n["qualified_name"]],"rationale":"No callers, no test references, no importers, not an entry point."}));
    }
    let mut stmt=conn.prepare("SELECT f.name,f.qualified_name,own.name,g.name FROM nodes f JOIN communities own ON own.id=f.community_id JOIN edges e ON e.kind='CALLS' AND e.target_qualified=f.qualified_name LEFT JOIN nodes c ON c.qualified_name=e.source_qualified LEFT JOIN communities g ON g.id=c.community_id WHERE f.kind='Function' AND f.extra NOT LIKE '%\"verilog_kind\"%' GROUP BY f.id HAVING count(DISTINCT g.id)=1 AND min(g.id)!=f.community_id ORDER BY f.id")?;
    for row in stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })? {
        let (name, qn, from, to) = row?;
        results.push(json!({"type":"move","description":format!("Move '{}' from '{from}' to '{to}'",clean(&name)),"symbols":[clean(&qn)],"rationale":format!("Function is in community '{from}' but only called by members of community '{to}'.")}));
    }
    Ok(results)
}

pub(super) fn run(conn: &Connection, root: &Path, args: &Value, hints: &Hints) -> Result<Value> {
    let mode = args["mode"].as_str().unwrap_or("rename");
    if !matches!(mode, "rename" | "dead_code" | "suggest") {
        return Ok(
            json!({"status":"error","error":format!("Invalid mode '{mode}'. Must be one of: dead_code, rename, suggest")}),
        );
    }
    let limit = positive(args, "max_results", 50, 150)?;
    if mode == "rename" {
        if text(args, "old_name").is_empty() || text(args, "new_name").is_empty() {
            return Ok(
                json!({"status":"error","error":"rename mode requires both old_name and new_name."}),
            );
        }
        return rename::preview(conn, root, args, hints, limit);
    }
    let (key, mut rows, fields) = if mode == "dead_code" {
        (
            "dead_code",
            dead::find(conn, root, text(args, "kind"), text(args, "file_pattern"))?,
            &["name", "kind", "relative_path", "line"][..],
        )
    } else {
        (
            "suggestions",
            suggestions(conn, root)?,
            &["type", "description", "symbols"][..],
        )
    };
    let total = rows.len();
    rows.truncate(limit);
    if args["detail_level"] == "minimal" {
        for row in &mut rows {
            *row = subset(row, fields);
        }
    }
    let mut summary = if mode == "dead_code" {
        format!("Found {total} dead code symbol(s)")
    } else {
        format!("Generated {total} refactoring suggestion(s)")
    };
    summary += &shown(rows.len(), total);
    summary.push('.');
    let mut out =
        json!({"status":"ok","summary":summary,key:rows,"total":total,"truncated":total>limit});
    hints.add(&mut out, "refactor_tool");
    Ok(out)
}

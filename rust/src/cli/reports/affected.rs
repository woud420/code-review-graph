use super::*;

pub(super) fn run(conn: &Connection, root: &Path, args: &Value, hints: &Hints) -> Result<Value> {
    let files = match super::super::changes::paths(root, args) {
        Ok(files) => files,
        Err(e) => return Ok(json!({"status":"error","error":e.to_string()})),
    };
    if files.is_empty() {
        return Ok(
            json!({"status":"ok","summary":"No changed files detected.","affected_flows":[],"total":0,"truncated":false}),
        );
    }
    let absolute: Vec<_> = files
        .iter()
        .map(|p| root.join(p.replace('\\', "/")))
        .collect();
    let filter = serde_json::to_string(&absolute)?;
    let mut statement=conn.prepare("SELECT f.* FROM flows f WHERE f.id IN (SELECT fm.flow_id FROM flow_memberships fm JOIN nodes n ON n.id=fm.node_id WHERE n.file_path IN (SELECT value FROM json_each(?1))) ORDER BY f.criticality DESC, f.id")?;
    let mut items = statement
        .query_map([filter], flow_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let total = items.len();
    let minimal = args["detail_level"] == "minimal";
    let cap = if minimal { 500 } else { 25 };
    let request = integer(args, "max_flows", 50)?;
    items.truncate(if request > 0 {
        (request as usize).min(cap)
    } else {
        cap
    });
    let mut truncated = items.len() < total;
    let mut budget = 400;
    for flow in &mut items {
        if minimal {
            *flow = subset(
                flow,
                &[
                    "id",
                    "name",
                    "criticality",
                    "depth",
                    "node_count",
                    "file_count",
                ],
            );
        } else {
            let mut steps = flow_steps(conn, flow)?;
            let count = steps.len();
            steps.truncate(budget);
            budget -= steps.len();
            let cut = count > steps.len();
            let created = flow
                .as_object_mut()
                .expect("flow")
                .shift_remove("created_at")
                .unwrap_or_default();
            let updated = flow
                .as_object_mut()
                .expect("flow")
                .shift_remove("updated_at")
                .unwrap_or_default();
            flow["steps"] = json!(steps);
            flow["created_at"] = created;
            flow["updated_at"] = updated;
            flow["total_steps"] = json!(count);
            if cut {
                flow["steps_omitted"] = json!(true);
                truncated = true;
            }
        }
    }
    let mut out = json!({"status":"ok","summary":format!("{total} flow(s) affected by changes in {} file(s){}",files.len(),shown(items.len(),total)),"changed_files":files,"affected_flows":items,"total":total,"truncated":truncated});
    hints.add(&mut out, "get_affected_flows_tool");
    Ok(out)
}

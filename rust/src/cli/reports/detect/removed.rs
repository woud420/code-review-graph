use super::*;
use code_review_graph_rs::{canonical::postprocess::RemovedFlowNode, model::Symbol};
use std::collections::BTreeMap;

pub(super) fn entry(
    conn: &Connection,
    path: &Path,
    qn: &str,
    symbol: &Symbol,
    churn: usize,
    callees: Vec<String>,
) -> Result<(Value, RemovedFlowNode)> {
    let (parent, name) = symbol
        .name
        .rsplit_once('.')
        .map_or(("", symbol.name.as_str()), |(p, n)| (p, n));
    let mut users = BTreeMap::new();
    let mut callers = Vec::new();
    let mut rows=conn.prepare("SELECT e.kind,e.source_qualified,e.target_qualified,e.line,e.file_path,e.extra FROM edges e WHERE e.kind!='CONTAINS' AND (e.target_qualified=?1 OR (e.source_qualified=?1 AND e.kind='TESTED_BY') OR (e.target_qualified=?2 AND e.kind='CALLS') OR json_extract(e.extra,'$.claimed_endpoint')=?1) ORDER BY id")?;
    let mut rows = rows.query(params![qn, name])?;
    while let Some(r) = rows.next()? {
        let kind: String = r.get(0)?;
        let source: String = r.get(1)?;
        let target: String = r.get(2)?;
        let extra: Value = serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or(Value::Null);
        if target == name && extra["claimed_endpoint"] != qn {
            let file: String = r.get(4)?;
            if Path::new(&file) != path
                || extra.get("ambiguous_targets").is_some()
                || extra.get("unresolved_targets").is_some()
            {
                continue;
            }
            let receiver = extra["receiver"].as_str();
            if parent.is_empty() && receiver.is_some()
                || !parent.is_empty()
                    && (!source.starts_with(&format!("{}::{parent}.", path.display()))
                        || receiver.is_some_and(|v| !matches!(v, "self" | "this" | "cls")))
            {
                continue;
            }
        }
        let user = if kind == "TESTED_BY" {
            &target
        } else {
            &source
        };
        if user == qn {
            continue;
        }
        if let Some(n) = conn
            .query_row(
                "SELECT name FROM nodes WHERE qualified_name=?1",
                [user],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            if kind == "CALLS" {
                callers.push(user.clone());
            }
            let line: Option<i64> = r.get(3)?;
            users.entry(user.clone()).or_insert_with(||json!({"name":clean(&n),"qualified_name":clean(user),"line":line,"relation":kind}));
        }
    }
    let kind = match symbol.kind.as_str() {
        "class" | "struct" | "interface" => "Class",
        "test" => "Test",
        _ => "Function",
    };
    let fake = json!({"id":-1,"kind":kind,"name":name,"qualified_name":qn,"file_path":path,"is_test":false});
    let file = path.to_string_lossy();
    let community:Option<i64>=conn.query_row("SELECT community_id FROM nodes WHERE file_path=?1 AND kind!='File' AND community_id IS NOT NULL GROUP BY community_id ORDER BY count(*) DESC,min(id) LIMIT 1",[&file],|r|r.get(0)).optional()?;
    let cross:i64=conn.query_row("SELECT count(*) FROM nodes WHERE community_id IS NOT NULL AND community_id!=?1 AND qualified_name IN(SELECT value FROM json_each(?2))",params![community,serde_json::to_string(&callers)?],|r|r.get(0))?;
    let score = if users.is_empty() {
        0.0
    } else {
        rounded(
            (risk::score(conn, &fake, path.parent().unwrap_or(path))?
                + (callers.len() as f64 / 20.).min(0.1)
                + (cross as f64 * 0.05).min(0.15)
                + churn_term(churn))
            .min(1.0),
        )
    };
    Ok((
        json!({"name":clean(name),"qualified_name":clean(qn),"kind":kind,"file":path,"old_line_start":symbol.start_line,"old_line_end":symbol.end_line,"risk_score":score,"used_by":users.values().take(3).collect::<Vec<_>>(),"used_by_total":users.len()}),
        RemovedFlowNode {
            name: name.into(),
            qualified_name: qn.into(),
            file: file.into_owned(),
            callers,
            callees,
        },
    ))
}

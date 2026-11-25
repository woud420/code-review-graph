use super::Node;
use anyhow::Result;
use rusqlite::{Transaction, params};
use serde_json::{Value, json};
use std::collections::HashMap;

pub(super) fn scoped_calls(tx: &Transaction<'_>, nodes: &[Node]) -> Result<usize> {
    let mut cpp = HashMap::<(String, String), Vec<&str>>::new();
    for n in nodes
        .iter()
        .filter(|n| n.language == "cpp" && matches!(n.kind.as_str(), "Function" | "Test"))
    {
        if let Some(scope) = n.parent.as_deref() {
            cpp.entry((scope.into(), n.name.clone()))
                .or_default()
                .push(&n.qn);
        }
    }
    if !nodes.iter().any(|n| n.language == "cpp") {
        return Ok(0);
    }
    for candidates in cpp.values_mut() {
        candidates.sort_unstable();
    }
    let mut query=tx.prepare("SELECT e.id,e.source_qualified,e.target_qualified,e.file_path,e.line,e.extra,s.parent_name
        FROM edges e JOIN nodes s ON s.qualified_name=e.source_qualified LEFT JOIN nodes t ON t.qualified_name=e.target_qualified
        WHERE e.kind='CALLS' AND s.language='cpp' AND ((t.id IS NULL AND instr(e.target_qualified,'::')>0)
        OR json_type(e.extra,'$.cpp_scoped_target')='text')")?;
    let cases = query
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut update = tx.prepare("UPDATE edges SET target_qualified=?,extra=? WHERE id=?")?;
    let mut mirror=tx.prepare("UPDATE edges SET source_qualified=?1,extra=?2 WHERE kind='TESTED_BY' AND target_qualified=?3 AND file_path=?4 AND line=?5
        AND (source_qualified IN (?6,?7) OR json_extract(extra,'$.cpp_scoped_target')=?7)")?;
    let mut resolved = 0;
    for (id, caller, endpoint, file, line, attributes, owner) in cases {
        let mut extra: Value = serde_json::from_str(&attributes)?;
        let target = extra["cpp_scoped_target"]
            .as_str()
            .unwrap_or(&endpoint)
            .to_owned();
        let Some((scope, name)) = target.trim_start_matches(':').rsplit_once("::") else {
            continue;
        };
        let explicit = scope.replace("::", ".");
        let mut parent = if target.starts_with("::") {
            None
        } else {
            owner.as_deref()
        };
        let matches = loop {
            let key = parent.map_or_else(|| explicit.clone(), |p| format!("{p}.{explicit}"));
            if let Some(group) = cpp.get(&(key, name.to_owned())) {
                break group.as_slice();
            }
            let Some(p) = parent else {
                break &[];
            };
            parent = p.rsplit_once('.').map(|(outer, _)| outer);
        };
        if let Some(values) = extra.as_object_mut() {
            for key in [
                "ambiguous_targets",
                "ambiguous_target_count",
                "ambiguous_targets_truncated",
                "unresolved_targets",
                "unresolved_target_count",
                "unresolved_targets_truncated",
            ] {
                values.remove(key);
            }
        }
        extra["cpp_scoped_target"] = json!(target);
        let desired = if let [only] = matches {
            *only
        } else {
            let prefix = if matches.is_empty() {
                "unresolved"
            } else {
                "ambiguous"
            };
            extra[format!("{prefix}_targets")] = json!(&matches[..matches.len().min(20)]);
            extra[format!("{prefix}_target_count")] = json!(matches.len());
            extra[format!("{prefix}_targets_truncated")] = json!(matches.len() > 20);
            &target
        };
        if matches.len() == 1
            && (desired != endpoint || serde_json::from_str::<Value>(&attributes)? != extra)
        {
            resolved += 1;
        }
        let metadata = extra.to_string();
        update.execute(params![desired, &metadata, id])?;
        mirror.execute(params![
            desired, &metadata, caller, file, line, endpoint, target
        ])?;
    }
    Ok(resolved)
}

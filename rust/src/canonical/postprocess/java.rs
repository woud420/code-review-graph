use anyhow::Result;
use rusqlite::{Transaction, params};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(super) fn resolve_framework_calls(tx: &Transaction<'_>) -> Result<()> {
    let mut reads=tx.prepare("SELECT c.id,c.target_qualified,c.extra,src.is_test,f.kind,f.target_qualified,json_extract(f.extra,'$.java_type')
        FROM edges f JOIN nodes owner ON owner.qualified_name=f.source_qualified AND owner.language='java'
        JOIN nodes src ON src.file_path=owner.file_path AND src.parent_name=owner.name
        JOIN edges c ON c.kind='CALLS' AND c.source_qualified=src.qualified_name
        WHERE f.kind IN ('INJECTS','TEMPORAL_STUB') AND json_extract(c.extra,'$.receiver')=json_extract(f.extra,'$.field_name')
        ORDER BY CASE f.kind WHEN 'INJECTS' THEN 0 ELSE 1 END, c.id")?;
    let mut implementors=tx.prepare("SELECT DISTINCT n.qualified_name FROM edges e JOIN nodes n ON n.qualified_name=e.source_qualified
        WHERE e.kind='INHERITS' AND n.language IN ('java','kotlin','scala') AND (?3 OR NOT n.is_test)
        AND CASE WHEN ?4 THEN e.target_qualified=?1 ELSE coalesce(json_extract(e.extra,'$.java_type'),e.target_qualified) IN (?1,?2) END")?;
    let mut classes=tx.prepare("SELECT qualified_name,json_extract(extra,'$.java_kind'),json_extract(extra,'$.temporal_role') FROM nodes
        WHERE language='java' AND kind='Class' AND name=?1 AND (?2=?1 OR (CASE WHEN coalesce(json_extract(extra,'$.java_package'),'')='' THEN name
        ELSE json_extract(extra,'$.java_package')||'.'||substr(qualified_name,instr(qualified_name,'::')+2) END)=?2) ORDER BY id")?;
    let mut method=tx.prepare("SELECT qualified_name FROM nodes WHERE file_path=?1 AND parent_name=?2 AND name=?3 AND language='java' AND kind IN ('Function','Test') ORDER BY id DESC LIMIT 1")?;
    let mut change = tx.prepare("UPDATE edges SET target_qualified=?1, extra=?2 WHERE id=?3")?;
    let mut rows = reads.query([])?;
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let old: String = r.get(1)?;
        let mut extra: Value = serde_json::from_str(&r.get::<_, String>(2)?)?;
        let test: bool = r.get(3)?;
        let temporal = r.get::<_, String>(4)? == "TEMPORAL_STUB";
        let injected: String = r.get(5)?;
        let java_type: Option<String> = r.get(6)?;
        let qualified_type = java_type.as_deref().unwrap_or(&injected);
        let bare = injected.rsplit('.').next().unwrap_or(&injected);
        let group = classes
            .query_map(params![bare, qualified_type], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if temporal
            && !group.iter().any(|(_, _, role)| {
                matches!(
                    role.as_deref(),
                    Some("activity_interface" | "workflow_interface")
                )
            })
        {
            continue;
        }
        let mut candidates = BTreeSet::new();
        if temporal || group.len() != 1 || group[0].1.as_deref() != Some("concrete") {
            candidates = implementors
                .query_map(
                    params![&injected, qualified_type, temporal || test, temporal],
                    |r| r.get::<_, String>(0),
                )?
                .collect::<rusqlite::Result<_>>()?;
        }
        let class = if candidates.len() == 1 {
            candidates.first().unwrap()
        } else if group.len() == 1 {
            &group[0].0
        } else {
            &injected
        };
        let member = old
            .rsplit("::")
            .next()
            .unwrap_or(&old)
            .rsplit('.')
            .next()
            .unwrap_or(&old);
        let mut target = format!("{class}.{member}");
        if let Some((path, local)) = class.split_once("::")
            && let Some(row) = method.query(params![path, local, member])?.next()?
        {
            target = row.get(0)?;
        }
        if temporal {
            extra["temporal_resolved"] = json!(true);
            extra["temporal_interface"] = json!(injected);
        } else {
            extra["spring_resolved"] = json!(true);
            extra["injected_type"] = json!(injected);
            if let Some(t) = java_type {
                extra["java_type"] = json!(t);
            }
        }
        change.execute(params![target, extra.to_string(), id])?;
    }
    Ok(())
}

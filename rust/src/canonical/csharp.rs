use super::qualified;
use anyhow::Result;
use rusqlite::{Connection, Transaction, params};
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path};

/// A binding can change when another file edits a global using, namespace, or type.
pub(super) fn rebind(tx: &Transaction<'_>, staging: &Connection, root: &Path) -> Result<()> {
    let changed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM affected WHERE file LIKE '%.cs')",
        [],
        |r| r.get(0),
    )?;
    if !changed {
        return Ok(());
    }
    let mut staged=staging.prepare("SELECT source_file,source,raw_target,line,target_file,target FROM call_sites WHERE source_file LIKE '%.cs'")?;
    let mut rows = staged.query([])?;
    let mut targets = HashMap::new();
    while let Some(r) = rows.next()? {
        let (file, caller, raw, line): (String, String, String, i64) =
            (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
        let (to, name): (String, String) = (r.get(4)?, r.get(5)?);
        targets.insert(
            (
                qualified(&root.join(file).to_string_lossy(), &caller),
                raw,
                line,
            ),
            qualified(&root.join(to).to_string_lossy(), &name),
        );
    }
    let mut current=tx.prepare("SELECT id,source_qualified,target_qualified,line,extra FROM edges WHERE kind='CALLS' AND file_path LIKE '%.cs' AND file_path NOT IN (SELECT file FROM affected)")?;
    let mut write = tx.prepare("UPDATE edges SET target_qualified=?,extra=? WHERE id=?")?;
    let mut mirror=tx.prepare("UPDATE edges SET source_qualified=?,extra=? WHERE kind='TESTED_BY' AND source_qualified=? AND target_qualified=? AND line=? AND json_extract(extra,'$.source_offset')=?")?;
    let mut rows = current.query([])?;
    while let Some(r) = rows.next()? {
        let (id, caller, previous, line): (i64, String, String, i64) =
            (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
        let mut extra: Value = serde_json::from_str(&r.get::<_, String>(4)?)?;
        let (Some(raw), Some(offset)) = (
            extra["csharp_raw_target"].as_str(),
            extra["source_offset"].as_i64(),
        ) else {
            continue;
        };
        let binding = targets.get(&(caller.clone(), format!("{raw}@{offset}"), line));
        let target = binding.map_or_else(|| raw.to_owned(), Clone::clone);
        if target == previous {
            continue;
        }
        if binding.is_some() {
            extra["scoped_resolved"] = json!(true);
            extra["scoped_via"] = json!("csharp_namespace");
            extra.as_object_mut().unwrap().remove("unresolved_targets");
        } else {
            extra["unresolved_targets"] = json!([]);
            let meta = extra.as_object_mut().unwrap();
            meta.remove("scoped_resolved");
            meta.remove("scoped_via");
        }
        let json = extra.to_string();
        write.execute(params![target, json, id])?;
        mirror.execute(params![target, json, previous, caller, line, offset])?;
    }
    Ok(())
}

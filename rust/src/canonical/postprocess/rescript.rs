use anyhow::Result;
use rusqlite::{Transaction, params};
use serde::Serialize;
use std::{collections::HashMap, path::Path};

#[derive(Debug, Default, Serialize)]
pub struct Summary {
    files_indexed: usize,
    calls_resolved: usize,
    imports_resolved: usize,
}

pub(super) fn resolve(tx: &Transaction<'_>) -> Result<Summary> {
    let mut modules = HashMap::<String, String>::new();
    let mut files = tx.prepare("SELECT DISTINCT file_path FROM nodes WHERE language='rescript' AND kind='File' ORDER BY file_path")?;
    for row in files.query_map([], |r| r.get::<_, String>(0))? {
        let path = row?;
        let file = Path::new(&path);
        if let Some(name) = file.file_stem().and_then(|v| v.to_str())
            && modules.get(name).is_none_or(|old| {
                Path::new(old)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("resi"))
            })
        {
            modules.insert(name.to_owned(), path);
        }
    }
    let mut summary = Summary {
        files_indexed: modules.len(),
        ..Default::default()
    };
    if modules.is_empty() {
        return Ok(summary);
    }

    let mut opens = HashMap::<String, Vec<String>>::new();
    let mut imports = tx.prepare("SELECT id,file_path,target_qualified,json_extract(extra,'$.rescript_import_kind') FROM edges WHERE kind='IMPORTS_FROM' ORDER BY id")?;
    let mut write = tx.prepare("UPDATE edges SET target_qualified=?1 WHERE id=?2")?;
    let mut rows = imports.query([])?;
    while let Some(row) = rows.next()? {
        let target: String = row.get(2)?;
        let root = target.split('.').next().unwrap_or(&target);
        if let Some(path) = modules.get(root) {
            let kind: Option<String> = row.get(3)?;
            if matches!(kind.as_deref(), Some("open" | "include")) {
                opens.entry(row.get(1)?).or_default().push(root.to_owned());
            }
            summary.imports_resolved += write.execute(params![path, row.get::<_, i64>(0)?])?;
        }
    }
    let mut calls = tx.prepare("SELECT id,file_path,target_qualified FROM edges WHERE kind='CALLS' AND instr(target_qualified,'::')=0 ORDER BY id")?;
    let mut exists = tx.prepare("SELECT EXISTS(SELECT 1 FROM nodes WHERE qualified_name=?)")?;
    let mut inferred = tx.prepare("UPDATE edges SET target_qualified=?1,extra=json_set(coalesce(extra,'{}'),'$.rescript_open_module',?2) WHERE id=?3")?;
    let mut rows = calls.query([])?;
    while let Some(row) = rows.next()? {
        let target: String = row.get(2)?;
        let id: i64 = row.get(0)?;
        if let Some((module, member)) = target.split_once('.') {
            if let Some(path) = modules.get(module) {
                let mut name = Some(member);
                while let Some(part) = name {
                    let qualified = format!("{path}::{part}");
                    if exists.query_row([&qualified], |r| r.get::<_, bool>(0))? {
                        summary.calls_resolved += write.execute(params![qualified, id])?;
                        break;
                    }
                    name = part.rsplit_once('.').map(|(owner, _)| owner);
                }
            }
        } else if let Some(opened) = opens.get(&row.get::<_, String>(1)?) {
            for module in opened {
                let qualified = format!("{}::{target}", modules[module]);
                if exists.query_row([&qualified], |r| r.get::<_, bool>(0))? {
                    summary.calls_resolved += inferred.execute(params![qualified, module, id])?;
                    break;
                }
            }
        }
    }
    Ok(summary)
}

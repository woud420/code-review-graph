use anyhow::Result;
use rusqlite::{Transaction, params};
use std::{collections::HashMap, path::Path};

struct Method {
    qualified: String,
    file: String,
}

fn segments(path: &str) -> Vec<&str> {
    path.split([':', '/', '\\'])
        .filter(|p| !p.is_empty())
        .collect()
}

fn matches_import(file: &str, target: &str, owner: &str) -> bool {
    let imported = segments(target);
    let Some((name, module)) = imported.split_last() else {
        return false;
    };
    if *name != owner || module.is_empty() {
        return false;
    }
    let path = Path::new(file).with_extension("");
    let parts = path.iter().filter_map(|p| p.to_str()).collect::<Vec<_>>();
    parts.ends_with(module)
        || module.len() > 1
            && matches!(module[0], "crate" | "self" | "super")
            && parts.ends_with(&module[1..])
}

fn supported<'a>(
    methods: &'a [Method],
    file: &str,
    owner: &str,
    enclosing: bool,
    imports: &[String],
) -> Option<(&'a str, &'static str)> {
    if let [method] = methods {
        if enclosing
            || method.file == file
            || imports
                .iter()
                .any(|i| i == &method.file || segments(i).last() == Some(&owner))
        {
            return Some((&method.qualified, "single_match"));
        }
        return None;
    }
    let unique = |matches: Vec<&'a Method>| {
        if matches.len() == 1 {
            Some((matches[0].qualified.as_str(), "import"))
        } else {
            None
        }
    };
    let exact = methods
        .iter()
        .filter(|m| imports.contains(&m.file))
        .collect();
    unique(exact).or_else(|| {
        unique(
            methods
                .iter()
                .filter(|m| imports.iter().any(|i| matches_import(&m.file, i, owner)))
                .collect(),
        )
    })
}

pub(super) fn resolve(tx: &Transaction<'_>) -> Result<()> {
    let mut methods = HashMap::<(String, String), Vec<Method>>::new();
    let mut sel=tx.prepare("SELECT parent_name,name,qualified_name,file_path FROM nodes WHERE language='rust' AND parent_name IS NOT NULL AND kind IN ('Function','Method','Test')")?;
    for row in sel.query_map([], |r| {
        Ok((
            (r.get(0)?, r.get(1)?),
            Method {
                qualified: r.get(2)?,
                file: r.get(3)?,
            },
        ))
    })? {
        let (key, method) = row?;
        methods.entry(key).or_default().push(method);
    }
    if methods.is_empty() {
        return Ok(());
    }
    let mut imports = HashMap::<String, Vec<String>>::new();
    let mut sel=tx.prepare("SELECT file_path,target_qualified FROM edges WHERE kind='IMPORTS_FROM' AND file_path GLOB '*.rs'")?;
    for row in sel.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
        let (file, target) = row?;
        imports.entry(file).or_default().push(target);
    }
    let mut sel=tx.prepare("SELECT id,source_qualified,target_qualified,file_path,line FROM edges WHERE kind='CALLS' AND file_path GLOB '*.rs' AND instr(target_qualified,'::')>0 AND instr(target_qualified,'/')=0")?;
    let mut update=tx.prepare("UPDATE edges SET target_qualified=?1,extra=json_set(extra,'$.scoped_resolved',json('true'),'$.scoped_via',?2),confidence_tier='INFERRED' WHERE id=?3")?;
    let mut mirror=tx.prepare("UPDATE edges SET source_qualified=?1,extra=(SELECT extra FROM edges WHERE id=?2),confidence_tier='INFERRED' WHERE kind='TESTED_BY' AND source_qualified=?3 AND target_qualified=?4 AND file_path=?5 AND line=?6")?;
    let pending = sel
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, caller, target, file, line) in pending {
        let Some((scope, name)) = target.split_once("::") else {
            continue;
        };
        if name.is_empty() || name.contains("::") || name.contains('.') || scope == "self" {
            continue;
        }
        let enclosing = scope == "Self";
        let owner = if enclosing {
            let Some((_, symbol)) = caller.split_once("::") else {
                continue;
            };
            let Some((owner, _)) = symbol.rsplit_once('.') else {
                continue;
            };
            owner
        } else {
            scope
        };
        let Some(candidates) = methods.get(&(owner.to_owned(), name.to_owned())) else {
            continue;
        };
        let imported = imports.get(&file).map(Vec::as_slice).unwrap_or_default();
        if let Some((new, via)) = supported(candidates, &file, owner, enclosing, imported) {
            update.execute(params![new, via, id])?;
            mirror.execute(params![new, id, target, caller, file, line])?;
        }
    }
    Ok(())
}

pub(super) fn settle_claims(tx: &Transaction<'_>) -> Result<()> {
    for (kind, column, managed) in [
        ("CALLS", "target_qualified", "bare_call_target"),
        ("TESTED_BY", "source_qualified", "bare_tested_by_source"),
    ] {
        let mut select=tx.prepare(&format!("SELECT e.id,e.{column},json_extract(e.extra,'$.{managed}') FROM edges e
            WHERE file_path GLOB '*.rs' AND kind=? AND {column} GLOB '/*::*' AND NOT EXISTS(SELECT 1 FROM nodes n WHERE n.qualified_name=e.{column})"))?;
        let mut update=tx.prepare(&format!("UPDATE edges SET {column}=?1,extra=CASE WHEN ?2 THEN json_set(extra,'$.claimed_endpoint',{column}) ELSE extra END WHERE id=?3"))?;
        let rows = select
            .query_map([kind], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, endpoint, raw) in rows {
            let leaf = endpoint.rsplit([':', '.']).next().unwrap_or("");
            if !leaf.is_empty() {
                update.execute(params![raw.as_deref().unwrap_or(leaf), raw.is_none(), id])?;
            }
        }
        tx.execute(&format!("UPDATE edges SET {column}=CASE WHEN EXISTS (SELECT 1 FROM nodes WHERE qualified_name=json_extract(edges.extra,'$.claimed_endpoint')) THEN json_extract(extra,'$.claimed_endpoint') ELSE {column} END,
            extra=json_remove(extra,'$.claimed_endpoint') WHERE kind=? AND file_path GLOB '*.rs' AND json_type(extra,'$.claimed_endpoint')='text'
            AND EXISTS(SELECT 1 FROM nodes WHERE qualified_name IN (edges.{column},json_extract(edges.extra,'$.claimed_endpoint')))"),[kind])?;
    }
    Ok(())
}

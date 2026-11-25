use super::{NodeRow, edge, qualified, write_node};
use crate::model::{ParsedFile, hcl::HclFacts};
use anyhow::Result;
use rusqlite::{Transaction, params};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

fn module_source(file: &Path, source: &str) -> Option<PathBuf> {
    if !(source.starts_with("./") || source.starts_with("../")) {
        return None;
    }
    let path = file.parent()?.join(source);
    if let Ok(path) = path.canonicalize() {
        return Some(path);
    }
    let mut parts = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir => {}
            part => parts.push(part),
        }
    }
    Some(parts)
}

#[derive(Default)]
pub(super) struct Modules {
    files: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
    names: BTreeMap<PathBuf, BTreeMap<String, BTreeSet<PathBuf>>>,
    imports: BTreeMap<PathBuf, Vec<PathBuf>>,
}

impl Modules {
    pub(super) fn load(root: &Path, staging: &rusqlite::Connection) -> Result<Self> {
        let mut modules = Self::default();
        let mut query = staging
            .prepare("SELECT path, facts FROM files WHERE path GLOB '*.tf' OR path GLOB '*.hcl'")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let relative: String = row.get(0)?;
            let json: String = row.get(1)?;
            let file = root.join(relative);
            let dir = file.parent().unwrap().to_owned();
            modules
                .files
                .entry(dir.clone())
                .or_default()
                .insert(file.clone());
            let facts: ParsedFile = serde_json::from_str(&json)?;
            let ns = modules.names.entry(dir).or_default();
            for name in facts.hcl.types.keys() {
                ns.entry(name.clone()).or_default().insert(file.clone());
            }
            modules.imports.insert(
                file.clone(),
                facts
                    .hcl
                    .modules
                    .iter()
                    .filter_map(|(src, _)| module_source(&file, src))
                    .collect(),
            );
        }
        Ok(modules)
    }

    pub(super) fn affected(&self, tx: &Transaction<'_>) -> Result<()> {
        if self.files.is_empty() {
            return Ok(());
        }
        let mut changed =
            tx.prepare("SELECT file FROM affected WHERE file GLOB '*.tf' OR file GLOB '*.hcl'")?;
        let dirs = changed
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|row| {
                row.map(|f| Path::new(&f).parent().map(Path::to_owned))
                    .transpose()
            })
            .collect::<rusqlite::Result<BTreeSet<_>>>()?;
        if dirs.is_empty() {
            return Ok(());
        }
        let mut save = tx.prepare_cached("INSERT OR IGNORE INTO affected(file) VALUES(?)")?;
        for (dir, paths) in &self.files {
            for path in paths {
                if dirs.contains(dir)
                    || self.imports.get(path).is_some_and(|targets| {
                        targets.iter().any(|p| {
                            dirs.contains(p) || p.parent().is_some_and(|d| dirs.contains(d))
                        })
                    })
                {
                    save.execute([path.to_string_lossy().as_ref()])?;
                }
            }
        }
        Ok(())
    }

    fn resolve<'a>(&'a self, file: &Path, name: &str) -> Option<&'a PathBuf> {
        let options = self.names.get(file.parent()?)?.get(name)?;
        options
            .get(file)
            .or_else(|| (options.len() == 1).then(|| options.first()).flatten())
    }

    fn import(&self, file: &Path, raw: &str) -> String {
        let Some(local) = module_source(file, raw) else {
            return raw.into();
        };
        if let Some(files) = self.files.get(&local) {
            let target = files.get(&local.join("main.tf")).or_else(|| files.first());
            if let Some(target) = target {
                return target.to_string_lossy().into_owned();
            }
        }
        if local
            .parent()
            .and_then(|dir| self.files.get(dir))
            .is_some_and(|e| e.contains(&local))
        {
            return local.to_string_lossy().into_owned();
        }
        raw.into()
    }

    pub(super) fn edges(&self, tx: &Transaction<'_>, path: &str, facts: &HclFacts) -> Result<()> {
        let file = Path::new(path);
        for (raw, line) in &facts.modules {
            edge(
                tx,
                "IMPORTS_FROM",
                path,
                &self.import(file, raw),
                path,
                *line,
                None,
            )?;
        }
        let mut save=tx.prepare_cached("INSERT OR IGNORE INTO imported(kind,source,target,path,line,resolution,extra) VALUES('REFERENCES',?,?,?,?,?,?)")?;
        for (owner, name, line) in &facts.references {
            let (target, resolution, extra) = if let Some(found) = self.resolve(file, name) {
                (
                    qualified(&found.to_string_lossy(), name),
                    "direct",
                    json!({}),
                )
            } else {
                (
                    name.rsplit('.').next().unwrap_or(name).to_owned(),
                    "unresolved",
                    json!({"claimed_endpoint":qualified(path,name)}),
                )
            };
            save.execute(params![
                qualified(path, owner),
                target,
                path,
                *line as i64,
                resolution,
                extra.to_string()
            ])?;
        }
        Ok(())
    }
}

pub(super) fn declarations(
    tx: &Transaction<'_>,
    path: &str,
    sha: &str,
    lines: usize,
    test_file: bool,
    file: &ParsedFile,
) -> Result<()> {
    write_node(
        tx,
        path,
        "hcl",
        sha,
        NodeRow {
            test_file,
            name: path,
            qualified: path,
            kind: "File",
            parent: None,
            start: 1,
            end: lines,
            declaration: None,
            extra: None,
        },
    )?;
    for s in &file.symbols {
        let name = &s.name;
        let qn = qualified(path, name);
        write_node(
            tx,
            path,
            "hcl",
            sha,
            NodeRow {
                test_file,
                name,
                qualified: &qn,
                kind: &s.kind,
                parent: None,
                start: s.start_line,
                end: s.end_line,
                declaration: None,
                extra: Some(&json!({"hcl_type":file.hcl.types[name]})),
            },
        )?;
        edge(tx, "CONTAINS", path, &qn, path, s.start_line, None)?;
    }
    Ok(())
}

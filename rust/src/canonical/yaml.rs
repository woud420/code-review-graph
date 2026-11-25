use super::{NodeRow, qualified, write_node};
use crate::model::ParsedFile;
use anyhow::Result;
use rusqlite::{Transaction, params};

pub(super) fn materialize(
    tx: &Transaction<'_>,
    path: &str,
    sha: &str,
    lines: usize,
    test_file: bool,
    facts: &ParsedFile,
) -> Result<()> {
    let yaml = &facts.yaml;
    if yaml.language.is_empty() {
        return Ok(());
    }
    write_node(
        tx,
        path,
        &yaml.language,
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
            extra: Some(&yaml.file),
        },
    )?;
    for symbol in &facts.symbols {
        let d = &yaml.declarations[&symbol.name];
        write_node(
            tx,
            path,
            &yaml.language,
            sha,
            NodeRow {
                test_file,
                name: &d.name,
                qualified: &qualified(path, &symbol.name),
                kind: &symbol.kind,
                parent: d.parent.as_deref(),
                start: symbol.start_line,
                end: symbol.end_line,
                declaration: None,
                extra: Some(&d.extra),
            },
        )?;
    }
    let mut save=tx.prepare_cached("INSERT OR IGNORE INTO imported(kind,source,target,path,line,resolution,extra) VALUES(?,?,?,?,?,?,?)")?;
    for e in &yaml.edges {
        let source = qualified(path, &e.source);
        let target = if e.local {
            qualified(path, &e.target)
        } else {
            e.target.clone()
        };
        let resolution = if e.kind == "CALLS" {
            Some(if e.local { "direct" } else { "unresolved" })
        } else {
            None
        };
        save.execute(params![
            e.kind,
            source,
            target,
            path,
            e.line as i64,
            resolution,
            e.extra.to_string()
        ])?;
    }
    Ok(())
}

use super::{Options, clean};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

struct Budget<'a> {
    root: &'a Path,
    files: HashMap<PathBuf, Vec<String>>,
    lines: usize,
    chars: usize,
    per_node: usize,
    omitted: usize,
}

impl Budget<'_> {
    fn attach(&mut self, node: &mut Value) -> bool {
        let Some(raw) = node["file_path"].as_str() else {
            return false;
        };
        let Some(start) = node["line_start"].as_u64().filter(|i| *i > 0) else {
            return false;
        };
        let Some(end) = node["line_end"].as_u64().filter(|i| *i >= start) else {
            return false;
        };
        if self.lines == 0 || self.chars == 0 {
            self.omitted += 1;
            return false;
        }
        let path = self.root.join(raw);
        let lines = self.files.entry(path.clone()).or_insert_with(|| {
            let Ok(path) = path.canonicalize() else {
                return vec![];
            };
            if !path.starts_with(self.root)
                || path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("ipynb"))
            {
                return vec![];
            }
            let Ok(bytes) = std::fs::read(&path) else {
                return vec![];
            };
            let text = String::from_utf8_lossy(&bytes)
                .replace("\r\n", "\n")
                .replace('\r', "\n");
            if text.lines().next() == Some("# Databricks notebook source") {
                return vec![];
            }
            text.split('\n').map(str::to_owned).collect()
        });
        let offset = (start - 1) as usize;
        if offset >= lines.len() {
            return false;
        }
        let span = ((end - start + 1) as usize)
            .min(self.per_node)
            .min(self.lines)
            .min(lines.len() - offset);
        let text = lines[offset..offset + span].join("\n");
        let cap = self.chars.min(12_000);
        let snippet = text.chars().take(cap).collect::<String>();
        let used = snippet.chars().count();
        let truncated = snippet.len() != text.len() || span < (end - start + 1) as usize;
        node["source"] = json!(snippet);
        node["source_lines"] = json!([start, start + snippet.matches('\n').count() as u64]);
        if truncated {
            node["source_truncated"] = json!(true);
        }
        self.chars -= used;
        self.lines -= span;
        true
    }
}

pub(super) fn attach(
    conn: &Connection,
    root: &Path,
    hits: &mut [Value],
    options: &Options,
) -> Result<usize> {
    let mut budget = Budget {
        root,
        files: HashMap::new(),
        lines: if options.expand_neighbors { 700 } else { 240 },
        chars: 80_000,
        per_node: options.max_source_lines.clamp(1, 60) as usize,
        omitted: 0,
    };
    let top = if options.expand_neighbors {
        hits.len().min(3)
    } else {
        hits.len()
    };
    for hit in &mut hits[..top] {
        budget.attach(hit);
    }
    if options.expand_neighbors {
        let mut node=conn.prepare("SELECT name,qualified_name,kind,file_path,line_start,line_end FROM nodes WHERE qualified_name=?")?;
        for hit in &mut hits[..top] {
            let displayed = hit["qualified_name"].as_str().unwrap_or_default();
            let q = if displayed.chars().count() < 256 {
                displayed.to_owned()
            } else {
                let mut rows = conn.prepare(
                    "SELECT qualified_name FROM nodes WHERE file_path=? AND line_start=?",
                )?;
                rows.query_map(
                    rusqlite::params![hit["file_path"].as_str(), hit["line_start"].as_i64()],
                    |r| r.get::<_, String>(0),
                )?
                .filter_map(std::result::Result::ok)
                .find(|name| clean(name, 256) == displayed)
                .unwrap_or_default()
            };
            for (field, sql) in [
                (
                    "callers",
                    "SELECT source_qualified FROM edges WHERE target_qualified=? AND kind='CALLS' ORDER BY id",
                ),
                (
                    "callees",
                    "SELECT target_qualified FROM edges WHERE source_qualified=? AND kind='CALLS' ORDER BY id",
                ),
            ] {
                let mut stmt = conn.prepare(sql)?;
                let mut entries = vec![];
                let mut seen = HashSet::new();
                for name in stmt.query_map([&q], |r| r.get::<_, String>(0))? {
                    if entries.len() >= 4 {
                        break;
                    }
                    if budget.chars == 0 || budget.lines == 0 {
                        budget.omitted += 1;
                        break;
                    }
                    let name = name?;
                    if name == q || !seen.insert(name.clone()) {
                        continue;
                    }
                    let row=node.query_row([name],|r|Ok(json!({"name":clean(&r.get::<_,String>(0)?,256),
                        "qualified_name":clean(&r.get::<_,String>(1)?,256),"kind":r.get::<_,String>(2)?,
                        "file_path":r.get::<_,String>(3)?,"line_start":r.get::<_,i64>(4)?,"line_end":r.get::<_,i64>(5)?}))).optional()?;
                    if let Some(mut entry) = row
                        && budget.attach(&mut entry)
                    {
                        entries.push(entry);
                    }
                }
                if !entries.is_empty() {
                    hit[field] = json!(entries);
                }
            }
        }
    }
    for hit in &mut hits[top..] {
        budget.attach(hit);
    }
    Ok(budget.omitted)
}

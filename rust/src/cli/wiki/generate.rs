use crate::cli::{
    query::{clean, text},
    reports,
};
use anyhow::{Result, ensure};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    fs,
    path::Path,
    sync::LazyLock,
};

fn cell(s: &str) -> String {
    static BREAKS: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\s*[\r\n]+\s*").unwrap());
    let safe = clean(s).replace('&', "&amp;").replace('|', "&#124;");
    let flat = BREAKS.replace_all(&safe, " ");
    let value = flat.trim();
    if value.is_empty() {
        "-".into()
    } else {
        value.into()
    }
}

fn dependencies(
    conn: &Connection,
    members: &HashSet<String>,
    qns: &[String],
    outgoing: bool,
) -> Result<Vec<(String, usize)>> {
    let (from, to) = if outgoing {
        ("source_qualified", "target_qualified")
    } else {
        ("target_qualified", "source_qualified")
    };
    let mut counts = HashMap::<String, (usize, usize)>::new();
    for chunk in qns.chunks(450) {
        let encoded = serde_json::to_string(chunk)?;
        let query = format!(
            "SELECT {to} FROM edges WHERE {from} IN (SELECT value FROM json_each(?)) ORDER BY {from},id"
        );
        for name in conn
            .prepare(&query)?
            .query_map([encoded], |r| r.get::<_, String>(0))?
        {
            let name = name?;
            if !members.contains(&name) {
                let order = counts.len();
                counts.entry(name).or_insert((0, order)).0 += 1;
            }
        }
    }
    let mut entries: Vec<_> = counts.into_iter().collect();
    entries.sort_by_key(|(_, (count, pos))| (std::cmp::Reverse(*count), *pos));
    Ok(entries
        .into_iter()
        .take(15)
        .map(|(k, (count, _))| (k, count))
        .collect())
}

fn page(conn: &Connection, group: &Value) -> Result<String> {
    let id = group["id"].as_i64().unwrap_or_default();
    let mut s = format!("# {}\n\n## Overview\n\n", cell(text(group, "name")));
    if !text(group, "description").is_empty() {
        writeln!(s, "{}\n", text(group, "description"))?;
    }
    writeln!(
        s,
        "- **Size**: {} nodes\n- **Cohesion**: {:.4}",
        group["size"],
        group["cohesion"].as_f64().unwrap_or_default()
    )?;
    if !text(group, "dominant_language").is_empty() {
        writeln!(
            s,
            "- **Dominant Language**: {}",
            text(group, "dominant_language")
        )?;
    }
    s.push_str("\n## Members\n\n");
    let mut qns = Vec::new();
    let mut rows = Vec::new();
    let mut members=conn.prepare("SELECT qualified_name,name,kind,file_path,line_start,line_end FROM nodes WHERE community_id=?")?;
    let mut items = members.query([id])?;
    while let Some(row) = items.next()? {
        qns.push(clean(&row.get::<_, String>(0)?));
        let kind: String = row.get(2)?;
        if qns.len() <= 50 && kind != "File" {
            let line = |i| {
                row.get::<_, Option<i64>>(i)
                    .map(|n| n.map_or_else(|| "None".to_owned(), |n| n.to_string()))
            };
            rows.push(format!(
                "| {} | {} | {} | {}-{} |",
                cell(&row.get::<_, String>(1)?),
                cell(&kind),
                cell(&row.get::<_, String>(3)?),
                line(4)?,
                line(5)?
            ));
        }
    }
    if !rows.is_empty() {
        writeln!(
            s,
            "| Name | Kind | File | Lines |\n|------|------|------|-------|\n{}",
            rows.join("\n")
        )?;
    } else if qns.is_empty() {
        s.push_str("No members found.\n");
    } else {
        s.push_str("No non-file members found.\n");
    }
    if qns.len() > 50 {
        writeln!(s, "\n*... and {} more members.*", qns.len() - 50)?;
    }
    s.push_str("\n## Execution Flows\n\n");
    let mut flows=conn.prepare("SELECT f.name,f.criticality,f.depth FROM (SELECT * FROM flows ORDER BY criticality DESC LIMIT 200) f
        WHERE EXISTS(SELECT 1 FROM flow_memberships m JOIN nodes n ON n.id=m.node_id WHERE m.flow_id=f.id AND n.qualified_name IN (SELECT value FROM json_each(?))) ORDER BY f.criticality DESC")?;
    let names = flows
        .query_map([serde_json::to_string(&qns)?], |r| {
            Ok(format!(
                "- **{}** (criticality: {:.2}, depth: {})",
                clean(&r.get::<_, String>(0)?),
                r.get::<_, f64>(1)?,
                r.get::<_, i64>(2)?
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if names.is_empty() {
        s.push_str("No execution flows pass through this community.\n");
    } else {
        writeln!(s, "{}", names[..names.len().min(10)].join("\n"))?;
        if names.len() > 10 {
            writeln!(s, "- *... and {} more flows.*", names.len() - 10)?;
        }
    }
    s.push_str("\n## Dependencies\n\n");
    let set = qns.iter().cloned().collect();
    let mut found = false;
    for (label, direction) in [("Outgoing", true), ("Incoming", false)] {
        let counts = dependencies(conn, &set, &qns, direction)?;
        if !counts.is_empty() {
            found = true;
            writeln!(s, "### {label}\n")?;
            for (name, count) in counts {
                writeln!(s, "- `{}` ({count} edge(s))", clean(&name))?;
            }
            s.push('\n');
        }
    }
    if !found {
        s.push_str("No cross-community dependencies detected.\n\n");
    }
    s.pop();
    Ok(s)
}

#[derive(Default)]
struct Pages {
    new: usize,
    updated: usize,
    unchanged: usize,
}

impl Pages {
    fn save(&mut self, path: &Path, body: String, force: bool, index: bool) -> Result<()> {
        ensure!(
            !path.is_symlink(),
            "wiki output points outside the generated files: {}",
            path.display()
        );
        if path.exists() && !force && fs::read(path)? == body.as_bytes() {
            self.unchanged += 1;
            return Ok(());
        }
        if path.exists() && !(force && index) {
            self.updated += 1;
        } else {
            self.new += 1;
        }
        fs::write(path, body)?;
        Ok(())
    }
}

pub(in crate::cli) fn write(conn: &Connection, db: &Path, force: bool) -> Result<Value> {
    let dir = db.with_file_name("wiki");
    ensure!(
        !dir.is_symlink(),
        "wiki output directory is a symbolic link"
    );
    fs::create_dir_all(&dir)?;
    let mut pages = Pages::default();
    let mut used = HashSet::new();
    let mut entries = Vec::new();
    for group in reports::group_rows(conn, "size DESC", 0)? {
        let name = text(&group, "name");
        let base = super::slug(name);
        let mut key = base.clone();
        let mut suffix = 2;
        while !used.insert(key.clone()) {
            key = format!("{base}-{suffix}");
            suffix += 1;
        }
        pages.save(
            &dir.join(format!("{key}.md")),
            page(conn, &group)?,
            force,
            false,
        )?;
        entries.push((name.to_owned(), key, group["size"].clone()));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut index = format!(
        "# Code Wiki\n\nAuto-generated documentation from the code knowledge graph community structure.\n\n**Total communities**: {}\n\n## Communities\n\n| Community | Size | Link |\n|-----------|------|------|\n",
        entries.len()
    );
    for (name, key, size) in entries {
        writeln!(index, "| {} | {size} | [{key}.md]({key}.md) |", cell(&name))?;
    }
    pages.save(&dir.join("index.md"), index, force, true)?;
    Ok(
        json!({"status":"ok","summary":format!("Wiki generated: {} new, {} updated, {} unchanged ({} total pages)",pages.new,pages.updated,pages.unchanged,pages.new+pages.updated+pages.unchanged),"wiki_dir":dir,"pages_generated":pages.new,"pages_updated":pages.updated,"pages_unchanged":pages.unchanged}),
    )
}

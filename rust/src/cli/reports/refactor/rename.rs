mod javascript;
mod python;
mod syntax;
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};
use syntax::{Edit, Lines, Scan, Source};

struct Target {
    name: String,
    file: String,
    qn: String,
    lang: String,
    first: usize,
    last: usize,
    member: bool,
}
impl Target {
    fn find(conn: &Connection, root: &Path, old: &str) -> Result<Option<Self>> {
        let absolute = root.join(old);
        let mut stmt=conn.prepare("SELECT name,file_path,qualified_name,coalesce(language,''),coalesce(line_start,0),coalesce(line_end,line_start,0),coalesce(parent_name,'')!='' FROM nodes WHERE kind!='File' AND (qualified_name IN(?1,?2) OR (instr(?1,'::')=0 AND (symbol=?1 OR name=?1))) ORDER BY CASE WHEN qualified_name IN(?1,?2) THEN 0 WHEN symbol=?1 THEN 1 ELSE 2 END,file_path,line_start LIMIT 1")?;
        Ok(stmt
            .query_row(params![old, absolute.to_string_lossy()], |r| {
                Ok(Self {
                    name: r.get(0)?,
                    file: r.get(1)?,
                    qn: r.get(2)?,
                    lang: r.get(3)?,
                    first: r.get::<_, i64>(4)? as usize,
                    last: r.get::<_, i64>(5)? as usize,
                    member: r.get(6)?,
                })
            })
            .optional()?)
    }
    fn window(&self, line: usize) -> bool {
        self.first <= line && line <= self.last.min(self.first + 20).max(self.first)
    }
}
fn js(lang: &str) -> bool {
    matches!(lang, "javascript" | "typescript" | "tsx" | "html")
}
type GraphLines = BTreeMap<String, Lines>;
fn graph(conn: &Connection, t: &Target) -> Result<GraphLines> {
    let mut lines = GraphLines::new();
    lines
        .entry(t.file.clone())
        .or_default()
        .insert(t.first, "definition");
    let mut stmt=conn.prepare("SELECT e.file_path,coalesce(e.line,0),e.kind,e.target_qualified,coalesce(n.language,''),e.extra FROM edges e LEFT JOIN nodes n ON n.qualified_name=e.source_qualified WHERE (e.target_qualified=?1 AND e.kind IN ('CALLS','REFERENCES','IMPORTS_FROM')) OR (e.target_qualified=?2 AND e.kind='CALLS') ORDER BY (e.target_qualified=?1) DESC,e.id")?;
    let mut rows = stmt.query(params![t.qn, t.name])?;
    while let Some(r) = rows.next()? {
        let dst: String = r.get(3)?;
        let lang: String = r.get(4)?;
        if dst != t.qn {
            if lang != t.lang && !(js(&lang) && js(&t.lang)) {
                continue;
            }
            let extra: Value = serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default();
            if extra["claimed_endpoint"]
                .as_str()
                .is_some_and(|n| n != t.qn)
            {
                continue;
            }
        }
        let label = match r.get::<_, String>(2)?.as_str() {
            "CALLS" => "call",
            "REFERENCES" => "reference",
            _ => "import",
        };
        lines
            .entry(r.get(0)?)
            .or_default()
            .entry(r.get::<_, i64>(1)? as usize)
            .or_insert(label);
    }
    Ok(lines)
}
fn collect(conn: &Connection, root: &Path, t: &Target, new: &str) -> Result<Option<Vec<Value>>> {
    let graph = graph(conn, t)?;
    let followed = super::apply::followed(conn)?;
    let mut stmt =
        conn.prepare("SELECT file_path,language FROM nodes WHERE kind='File' ORDER BY file_path")?;
    let mut files = BTreeMap::new();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let lang: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
        let family = t.lang == "python" && lang == "python" || js(&t.lang) && js(&lang);
        if (graph.contains_key(&path) || (!t.member && family))
            && Path::new(&path).starts_with(root)
            && Path::new(&path)
                .canonicalize()
                .is_ok_and(|f| f.starts_with(root) || followed.contains(&path))
            && let Some(src) = Source::read(&path, &lang, &t.name, graph.contains_key(&path))
        {
            files.insert(path, src);
        }
    }
    let mut imports = conn.prepare(
        "SELECT file_path,line,target_qualified FROM edges WHERE kind='IMPORTS_FROM' ORDER BY id",
    )?;
    let mut edges = imports.query([])?;
    while let Some(e) = edges.next()? {
        if let Some(src) = files.get_mut(&e.get::<_, String>(0)?) {
            src.imports
                .entry(e.get::<_, i64>(1)? as usize)
                .or_default()
                .insert(e.get(2)?);
        }
    }
    let mut exporters = BTreeSet::new();
    let mut scans = BTreeMap::new();
    let scoped = files.get(&t.file).is_some_and(|src| {
        src.tree.as_ref().is_some_and(|tree| {
            if t.lang == "python" {
                python::definition(tree.root_node(), src, t).is_some_and(|(_, method)| !method)
            } else {
                js(&t.lang) && !t.member
            }
        })
    });
    if scoped {
        // A newly discovered re-export can bind the name in a previously scanned importer.
        loop {
            let count = exporters.len();
            for (path, src) in &files {
                let Some(tree) = &src.tree else { continue };
                let scan = if src.lang == "python" && t.lang == "python" {
                    python::scan(tree.root_node(), src, t, &exporters)
                } else if js(&src.lang) && js(&t.lang) {
                    javascript::scan(tree.root_node(), src, t, new, &exporters)
                } else {
                    continue;
                };
                if scan.exports {
                    exporters.insert(path.clone());
                }
                scans.insert(path.clone(), scan);
            }
            if exporters.len() == count {
                break;
            }
        }
    }
    let mut edits = Vec::new();
    let paths: BTreeSet<_> = graph.keys().chain(scans.keys()).cloned().collect();
    for path in &paths {
        let mut scan = scans.remove(path).unwrap_or_default();
        let lines = graph.get(path);
        if let Some(src) = files.get(path) {
            if !scan.authoritative
                && let Some(lines) = lines
            {
                syntax::fallback(src, t, new, lines, scoped, &mut scan);
            }
            if path == &t.file && !scan.edits.values().any(|e| e.site == "definition") {
                return Ok(None);
            }
        } else if let Some(lines) = lines {
            for (&line, &site) in lines {
                scan.edits.insert((line, 0), Edit { site, new: None });
            }
        }
        for ((line, col), edit) in scan.edits {
            let mut v = json!({"file":path,"line":line});
            if col > 0 {
                v["column"] = json!(col);
            }
            v["old"] = json!(t.name);
            v["new"] = json!(edit.new.as_deref().unwrap_or(new));
            v["site"] = json!(edit.site);
            v["confidence"] = json!("high");
            edits.push(v);
        }
    }
    Ok(Some(edits))
}
pub(super) fn preview(
    conn: &Connection,
    root: &Path,
    args: &Value,
    hints: &Hints,
    limit: usize,
) -> Result<Value> {
    let (old, new) = (text(args, "old_name"), text(args, "new_name"));
    let absent =
        || json!({"status":"not_found","summary":format!("No node found matching '{old}'.")});
    let Some(target) = Target::find(conn, root, old)? else {
        eprintln!("WARNING: rename_preview: node '{old}' not found");
        return Ok(absent());
    };
    let Some(mut edits) = collect(conn, root, &target, new)? else {
        eprintln!(
            "WARNING: rename_preview: {} is not at {}:{} any more; the graph is stale",
            target.name, target.file, target.first
        );
        return Ok(absent());
    };
    let total = edits.len();
    let id = hints.remember_refactor(conn, root, &edits)?;
    let time = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
    eprintln!("INFO: rename_preview: created refactor {id} ({old} -> {new}, {total} edits)");
    edits.truncate(limit);
    if args["detail_level"] == "minimal" {
        for e in &mut edits {
            *e = subset(e, &["file", "line", "confidence"]);
        }
    }
    let summary = format!(
        "Rename preview: {old} -> {new}, {total} edit(s){}. Use apply_refactor_tool(refactor_id='{id}') to apply.",
        shown(edits.len(), total)
    );
    let mut out = json!({"status":"ok","summary":summary,"refactor_id":id,"type":"rename","old_name":clean(old),"new_name":clean(new),"qualified_name":clean(&target.qn),"edits":edits,"stats":{"high":total,"medium":0,"low":0},"created_at":time,"total":total,"truncated":total>limit});
    hints.add(&mut out, "refactor_tool");
    Ok(out)
}

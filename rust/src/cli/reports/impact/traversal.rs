use super::*;

pub(in crate::cli::reports) struct Radius {
    pub changed: Vec<Value>,
    pub impacted: Vec<Value>,
    pub files: Vec<String>,
    pub edges: Vec<Value>,
    pub total: usize,
    pub unresolved: i64,
    pub resolved: Vec<String>,
    pub unmatched: Vec<String>,
}

fn config(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|n| n.parse::<f64>().ok())
        .filter(|x| (0.0..=1.0).contains(x))
        .unwrap_or(default)
}

pub(in crate::cli::reports) fn run(
    conn: &Connection,
    root: &Path,
    inputs: &[String],
    depth: usize,
    cap: usize,
    direct: bool,
) -> Result<Radius> {
    let mut resolved = Vec::new();
    let mut unmatched = Vec::new();
    let mut files=conn.prepare("SELECT DISTINCT file_path FROM nodes WHERE substr(lower(file_path),-length(?1))=lower(?1) ORDER BY file_path")?;
    for f in inputs {
        let raw = f.replace('\\', "/");
        let path = Path::new(&raw);
        let alternate = if path.is_absolute() {
            path.strip_prefix(root).unwrap_or(path).to_path_buf()
        } else {
            root.join(path)
        };
        let mut found = false;
        for cand in [raw, alternate.to_string_lossy().into_owned()] {
            for p in files.query_map([cand], |r| r.get::<_, String>(0))? {
                let p = p?;
                found = true;
                if !resolved.contains(&p) {
                    resolved.push(p);
                }
            }
        }
        if !found && !unmatched.contains(f) {
            unmatched.push(clean(f).replace('\n', " "));
        }
    }
    conn.execute_batch("CREATE TEMP TABLE _impact_seeds(qn TEXT PRIMARY KEY);
        CREATE TEMP TABLE _impact_best(node_qn TEXT PRIMARY KEY,score REAL);
        CREATE TEMP TABLE _impact_frontier(node_qn TEXT PRIMARY KEY,score REAL);
        CREATE TEMP TABLE _impact_next(node_qn TEXT PRIMARY KEY,score REAL);
        CREATE TEMP TABLE _impact_frontier_dirs(dir TEXT PRIMARY KEY,score REAL);
        CREATE TEMP TABLE _impact_policies(kind TEXT PRIMARY KEY,weight REAL,direction TEXT);
        INSERT INTO _impact_policies VALUES('CALLS',1,'incoming'),('INHERITS',0.9,'incoming'),('OVERRIDES',0.9,'incoming'),('IMPLEMENTS',0.9,'incoming'),('TESTED_BY',0.7,'outgoing'),('REFERENCES',0.6,'incoming'),('DEPENDS_ON',0.6,'incoming'),('IMPORTS_FROM',0.5,'incoming'),('CONTAINS',0.3,'none');")?;
    let json_files = serde_json::to_string(&resolved)?;
    conn.execute("INSERT INTO _impact_seeds SELECT qualified_name FROM nodes WHERE file_path IN(SELECT value FROM json_each(?1))",[&json_files])?;
    conn.execute("INSERT OR IGNORE INTO _impact_seeds SELECT j.value FROM nodes n,json_each(n.extra,'$.csharp_namespaces') j WHERE n.kind='File' AND n.language='csharp' AND j.type='text' AND n.file_path IN(SELECT value FROM json_each(?1))",[&json_files])?;
    for f in &resolved {
        for p in Path::new(f).ancestors().skip(2).take(12) {
            if let Some(p) = p.to_str() {
                conn.execute("INSERT OR IGNORE INTO _impact_seeds SELECT ?1 WHERE EXISTS(SELECT 1 FROM edges WHERE kind='IMPORTS_FROM' AND target_qualified=?1 AND json_extract(extra,'$.import_scope')='tree')",[p])?;
            }
        }
    }
    conn.execute_batch("INSERT INTO _impact_best SELECT qn,1.0 FROM _impact_seeds; INSERT INTO _impact_frontier SELECT * FROM _impact_best;")?;
    let decay = config("CRG_IMPACT_DEPTH_DECAY", 0.6);
    let floor = config("CRG_IMPACT_SCORE_FLOOR", 0.05);
    for _ in 0..depth {
        conn.execute_batch("DELETE FROM _impact_next; DELETE FROM _impact_frontier_dirs;")?;
        let mut stmt=conn.prepare("SELECT n.file_path,f.score FROM _impact_frontier f JOIN nodes n ON n.qualified_name=f.node_qn AND n.kind='File'")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let path: String = r.get(0)?;
            let score: f64 = r.get(1)?;
            if let Some(p) = Path::new(&path).parent().and_then(Path::to_str) {
                conn.execute("INSERT INTO _impact_frontier_dirs VALUES(?1,?2) ON CONFLICT(dir) DO UPDATE SET score=max(score,excluded.score)",params![p,score])?;
            }
        }
        conn.execute(
            include_str!("../impact_step.sql"),
            params![decay, direct, floor],
        )?;
        conn.execute("DELETE FROM _impact_next WHERE score<=coalesce((SELECT score FROM _impact_best WHERE _impact_best.node_qn=_impact_next.node_qn),0)",[])?;
        if !conn.query_row("SELECT EXISTS(SELECT 1 FROM _impact_next)", [], |r| {
            r.get::<_, bool>(0)
        })? {
            break;
        }
        conn.execute_batch("INSERT OR REPLACE INTO _impact_best SELECT * FROM _impact_next;DELETE FROM _impact_frontier;INSERT INTO _impact_frontier SELECT * FROM _impact_next;")?;
    }
    let changed=conn.prepare("SELECT * FROM nodes WHERE qualified_name IN(SELECT qn FROM _impact_seeds) ORDER BY qualified_name")?.query_map([],node)?.collect::<rusqlite::Result<Vec<_>>>()?;
    let query = "FROM _impact_best b JOIN nodes n ON n.qualified_name=b.node_qn WHERE b.node_qn NOT IN(SELECT qn FROM _impact_seeds) AND n.extra NOT LIKE '%\"verilog_kind\"%'";
    let total = conn.query_row(&format!("SELECT count(*) {query}"), [], |r| {
        r.get::<_, i64>(0)
    })? as usize;
    let impacted = conn
        .prepare(&format!(
            "SELECT n.*,b.score AS impact_score {query} ORDER BY score DESC, b.node_qn LIMIT ?"
        ))?
        .query_map([cap as i64], |r| {
            let mut n = node(r)?;
            let score: f64 = r.get("impact_score")?;
            n["impact_score"] = json!(
                format!("{score:.4}")
                    .parse::<f64>()
                    .expect("finite impact score")
            );
            Ok(n)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    conn.execute_batch("CREATE TEMP TABLE _impact_kept(qn TEXT PRIMARY KEY);INSERT INTO _impact_kept SELECT qn FROM _impact_seeds")?;
    for n in &impacted {
        conn.execute(
            "INSERT OR IGNORE INTO _impact_kept VALUES(?)",
            [text(n, "qualified_name")],
        )?;
    }
    let mut edges=conn.prepare("SELECT e.* FROM edges e WHERE source_qualified IN(SELECT qn FROM _impact_kept) AND target_qualified IN(SELECT qn FROM _impact_kept) ORDER BY source_qualified,id")?.query_map([],|r|Ok(super::super::super::query::edge(r)?.wire))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut seen_edges = edges
        .iter()
        .map(|e| e["id"].as_i64())
        .collect::<HashSet<_>>();
    for f in changed
        .iter()
        .chain(&impacted)
        .map(|n| text(n, "file_path"))
        .collect::<HashSet<_>>()
    {
        for (depth, p) in Path::new(f).ancestors().skip(1).take(13).enumerate() {
            let mut stmt=conn.prepare("SELECT e.* FROM edges e WHERE source_qualified IN(SELECT qn FROM _impact_kept) AND target_qualified=?1 AND kind='IMPORTS_FROM' AND (json_extract(extra,'$.import_scope')='tree' OR (?2 AND json_extract(extra,'$.import_scope')='package')) ORDER BY id")?;
            for e in stmt.query_map(params![p.to_string_lossy(), depth == 0], |r| {
                Ok(super::super::super::query::edge(r)?.wire)
            })? {
                let e = e?;
                if seen_edges.insert(e["id"].as_i64()) {
                    edges.push(e);
                }
            }
        }
    }
    if direct {
        edges.retain(|e| e["target_resolution"] != "unresolved");
    }
    let mut impacted_files = Vec::new();
    for n in &impacted {
        let file = text(n, "file_path");
        if !impacted_files.iter().any(|x| x == file) {
            impacted_files.push(file.to_string());
        }
    }
    let unresolved=conn.query_row("SELECT count(*) FROM edges WHERE kind IN('CALLS','REFERENCES') AND target_qualified IN(SELECT name FROM nodes WHERE qualified_name IN(SELECT qn FROM _impact_seeds) AND kind!='File') AND coalesce(target_resolution,CASE WHEN EXISTS(SELECT 1 FROM nodes WHERE qualified_name=edges.target_qualified) THEN 'direct' ELSE 'unresolved' END)='unresolved'",[],|r|r.get(0))?;
    Ok(Radius {
        changed,
        impacted,
        files: impacted_files,
        edges,
        total,
        unresolved,
        resolved,
        unmatched,
    })
}

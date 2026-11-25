use anyhow::{Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
};

pub fn project_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|dir| {
            [".git", ".svn", ".code-review-graph"]
                .iter()
                .any(|marker| dir.join(marker).exists())
        })
        .map(Path::to_path_buf)
}

pub fn database(root: &Path, data_dir: Option<&Path>) -> PathBuf {
    data_dir
        .and_then(|p| crate::cli::absolute(p).ok())
        .or_else(|| {
            let entries=crate::cli::registry::list().ok()?;
            let entry=entries.iter().find(|v| v["path"].as_str()==root.to_str())?;
            let dir=crate::cli::absolute(Path::new(entry["data_dir"].as_str()?)).ok()?;
            if !dir.exists() && root.join(".code-review-graph/graph.db").is_file() {
                eprintln!("WARNING: Registered graph directory {} is missing; falling back to normal resolution (local graph: {}).",dir.display(),root.join(".code-review-graph").display());
                None
            }else{Some(dir)}
        })
        .or_else(|| {
            std::env::var_os("CRG_DATA_DIR")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| root.join(".code-review-graph"))
        .join("graph.db")
}

pub fn open(db: &Path) -> Result<Connection> {
    ensure!(
        db.is_file(),
        "No graph found at {}. Run `code-review-graph build` first.",
        db.display()
    );
    code_review_graph_rs::canonical::database::permissions(db)?;
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    code_review_graph_rs::canonical::database::validate(&conn, db)?;
    code_review_graph_rs::canonical::database::migrate::before_read(db, &conn)?;
    conn.execute_batch("BEGIN DEFERRED")?;
    Ok(conn)
}

pub fn metadata(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM metadata WHERE key=?1", [key], |r| {
            r.get(0)
        })
        .optional()?)
}

fn groups(conn: &Connection, sql: &str) -> Result<BTreeMap<String, i64>> {
    let mut stmt = conn.prepare(sql)?;
    Ok(stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

pub struct Counts {
    pub node_kinds: BTreeMap<String, i64>,
    pub edge_kinds: BTreeMap<String, i64>,
    pub languages: Vec<String>,
    pub updated: Option<String>,
}

impl Counts {
    pub fn read(conn: &Connection) -> Result<Self> {
        let node_kinds = groups(
            conn,
            "SELECT CASE WHEN extra LIKE '%\"verilog_kind\"%' THEN 'Signal' ELSE kind END AS k,COUNT(*) FROM nodes GROUP BY k",
        )?;
        let edge_kinds = groups(conn, "SELECT kind,COUNT(*) FROM edges GROUP BY kind")?;
        let mut stmt = conn.prepare("SELECT DISTINCT language FROM nodes WHERE kind='File' AND language IS NOT NULL AND language!='' ORDER BY language")?;
        let languages = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Self {
            node_kinds,
            edge_kinds,
            languages,
            updated: metadata(conn, "last_updated")?,
        })
    }

    pub fn nodes(&self) -> i64 {
        self.node_kinds.values().sum()
    }
    pub fn edges(&self) -> i64 {
        self.edge_kinds.values().sum()
    }
    pub fn files(&self) -> i64 {
        self.node_kinds.get("File").copied().unwrap_or_default()
    }
}

pub fn git(root: &Path, args: &[&str]) -> String {
    Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default()
}

pub fn status(root: &Path, conn: &Connection, output_json: bool, quiet: bool) -> Result<()> {
    let stats = Counts::read(conn)?;
    let vcs = root
        .ancestors()
        .find_map(|p| {
            if p.join(".git").exists() {
                Some("git")
            } else if p.join(".svn").exists() {
                Some("svn")
            } else {
                None
            }
        })
        .unwrap_or("none");
    let branch = (vcs == "git").then(|| git(root, &["rev-parse", "--abbrev-ref", "HEAD"]));
    let sha = (vcs == "git").then(|| git(root, &["rev-parse", "HEAD"]));
    let built_branch = metadata(conn, "git_branch")?;
    let built_sha = metadata(conn, "git_head_sha")?;
    let svn_branch = metadata(conn, "svn_branch")?;
    let svn_rev = metadata(conn, "svn_revision")?;
    let build_state = metadata(conn, "build_state")?;
    let incomplete = matches!(
        build_state.as_deref(),
        Some("in-progress" | "postprocess-pending")
    );
    if output_json {
        let data = json!({"nodes":stats.nodes(), "edges":stats.edges(), "files":stats.files(), "languages":stats.languages,
            "last_updated":stats.updated, "vcs":vcs, "built_on_branch":built_branch, "built_at_commit":built_sha, "current_branch":branch,
            "current_sha":sha, "svn_branch":svn_branch, "svn_revision":svn_rev, "build_incomplete":incomplete});
        println!("{}", crate::cli::wire::json_text(&data, false));
    } else if !quiet {
        println!(
            "Nodes: {}\nEdges: {}\nFiles: {}\nLanguages: {}\nLast updated: {}",
            stats.nodes(),
            stats.edges(),
            stats.files(),
            stats.languages.join(", "),
            stats.updated.as_deref().unwrap_or("never")
        );
        if incomplete {
            println!(
                "{}",
                if build_state.as_deref() == Some("in-progress") {
                    "Build state: INCOMPLETE - the last build stopped before every file was stored, so files are missing from the graph. Run 'code-review-graph build' to rebuild it."
                } else {
                    "Build state: INCOMPLETE - the last build stopped before post-processing finished, so search and flows may be missing. Run 'code-review-graph update' to repair it."
                }
            );
        }
        if let Some(b) = built_branch.as_ref().filter(|b| !b.is_empty()) {
            println!("Built on branch: {b}");
        }
        if let Some(sha) = built_sha.as_ref().filter(|s| !s.is_empty()) {
            println!(
                "Built at commit: {}",
                sha.chars().take(12).collect::<String>()
            );
        }
        if let (Some(built), Some(current)) = (built_branch, branch)
            && !built.is_empty()
            && !current.is_empty()
            && built != current
        {
            println!(
                "WARNING: Graph was built on '{built}' but you are now on '{current}'. Run 'code-review-graph build' to rebuild."
            );
        }
        if vcs == "svn" {
            if let Some(b) = svn_branch.filter(|b| !b.is_empty()) {
                println!("SVN branch: {b}");
            }
            if let Some(r) = svn_rev.filter(|r| !r.is_empty()) {
                println!("SVN revision at build: {r}");
            }
        }
    }
    Ok(())
}

pub fn tool_stats(conn: &Connection, root: &Path) -> Result<Value> {
    let stats = Counts::read(conn)?;
    let splits = groups(
        conn,
        "SELECT COALESCE(target_resolution, CASE WHEN NOT (json_type(extra,'$.ambiguous_targets') IS NOT NULL OR json_type(extra,'$.unresolved_targets') IS NOT NULL) AND EXISTS(SELECT 1 FROM nodes WHERE qualified_name=edges.target_qualified) THEN 'direct' ELSE 'unresolved' END) AS k,count(*) FROM edges WHERE kind='CALLS' GROUP BY k",
    )?;
    let direct = splits.get("direct").copied().unwrap_or(0);
    let unresolved = splits.get("unresolved").copied().unwrap_or(0);
    let embedded = embedding_count(conn)?;
    let langs = if stats.languages.is_empty() {
        "none".to_owned()
    } else {
        stats.languages.join(", ")
    };
    let mut summary = format!(
        "Graph statistics for {}:\n  Files: {}\n  Total nodes: {}\n  Total edges: {}\n  Languages: {langs}\n  Last updated: {}\n\nNodes by kind:",
        root.file_name().unwrap_or_default().to_string_lossy(),
        stats.files(),
        stats.nodes(),
        stats.edges(),
        stats.updated.as_deref().unwrap_or("never")
    );
    for (kind, n) in &stats.node_kinds {
        summary.push_str(&format!("\n  {kind}: {n}"));
    }
    summary.push_str("\n\nEdges by kind:");
    for (kind, n) in &stats.edge_kinds {
        summary.push_str(&format!("\n  {kind}: {n}"));
    }
    summary.push_str(&format!("\n\nCALLS by target resolution: {direct} direct, {unresolved} unresolved\n\nEmbeddings: {embedded} nodes embedded"));
    if !code_review_graph_rs::canonical::embeddings::available()? {
        summary.push_str("\n  (install sentence-transformers for semantic search)");
    }
    Ok(
        json!({"status":"ok","summary":summary,"total_nodes":stats.nodes(),"total_edges":stats.edges(),"nodes_by_kind":stats.node_kinds,"edges_by_kind":stats.edge_kinds,
        "calls_by_resolution":{"direct":direct,"unresolved":unresolved},"languages":stats.languages,"files_count":stats.files(),"last_updated":stats.updated,"embeddings_count":embedded}),
    )
}

pub(in crate::cli) fn embedding_count(conn: &Connection) -> Result<i64> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='embeddings')",
        [],
        |r| r.get(0),
    )?;
    Ok(if exists {
        conn.query_row("SELECT COUNT(*) FROM embeddings", [], |r| {
            r.get::<_, i64>(0)
        })?
    } else {
        0
    })
}

pub fn check_root(conn: &Connection, db: &Path, root: &Path) -> Result<()> {
    let mut stmt = conn.prepare("SELECT file_path FROM nodes WHERE kind='File' ORDER BY id")?;
    let mut count = 0;
    let mut elsewhere = None;
    for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
        let file = PathBuf::from(row?);
        if !file.is_absolute() {
            continue;
        }
        if file.starts_with(root) {
            return Ok(());
        }
        count += 1;
        if count <= 25
            && let Ok(path) = file.canonicalize()
        {
            if path.starts_with(root) {
                return Ok(());
            }
            if elsewhere.is_none() {
                elsewhere = Some(path);
            }
        }
    }
    if let Some(file) = elsewhere {
        anyhow::bail!(
            "the graph at {} was built for a different repository root: none of its {count} file(s), such as {}, are under {}. Run `code-review-graph build` here, or point --repo at the root it was built for.",
            db.display(),
            file.display(),
            root.display()
        );
    }
    Ok(())
}

pub fn provenance(conn: &Connection, root: &Path) -> Result<Value> {
    let mut result = json!({});
    if let Some(updated) = metadata(conn, "last_updated")?.filter(|s| !s.is_empty()) {
        result["updated_at"] = json!(updated);
        let age:Option<i64>=conn.query_row("SELECT MAX(0,CAST((julianday('now')-julianday(?1,CASE WHEN substr(?1,-1)='Z' OR instr(substr(?1,12),'+') OR instr(substr(?1,12),'-') THEN '+0 seconds' ELSE 'utc' END))*86400 AS INTEGER))",[updated],|r|r.get(0))?;
        if let Some(a) = age {
            result["age_seconds"] = json!(a);
        }
    }
    let built = metadata(conn, "git_head_sha")?.filter(|s| !s.is_empty());
    if let Some(sha) = &built {
        result["built_at_sha"] = json!(sha);
    }
    if result.as_object().is_some_and(|o| !o.is_empty()) {
        if let Some(branch) = metadata(conn, "git_branch")?.filter(|b| !b.is_empty()) {
            result["built_on_branch"] = json!(branch);
        }
        let sha = git(root, &["rev-parse", "HEAD"]);
        if !sha.is_empty() {
            result["head_sha"] = json!(sha);
            if let Some(built) = built {
                result["head_matches_build"] = json!(built == sha);
            }
        }
    }
    Ok(result)
}

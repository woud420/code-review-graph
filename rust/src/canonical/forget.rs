use super::{
    Graph, ParsedFile, Source, WriteLock, Writer, begin_edges, evidence, metadata, postprocess,
    replace_edges, set_metadata,
};
use anyhow::{Context, Result, ensure};
use globset::GlobBuilder;
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

pub struct Outcome {
    pub files: Vec<String>,
    pub tracked: usize,
    pub reparsed: usize,
    pub derived: Option<postprocess::Report>,
}

pub(super) fn unchanged_omissions(
    db: &Connection,
    root: &Path,
    rebuild: bool,
) -> Result<BTreeSet<String>> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='rust_forgotten')",
        [],
        |r| r.get(0),
    )?;
    let mut omitted = BTreeSet::new();
    if !exists {
        return Ok(omitted);
    }
    if rebuild {
        db.execute("DELETE FROM rust_forgotten", [])?;
        return Ok(omitted);
    }
    let mut stmt = db.prepare("SELECT relative_path,sha256 FROM rust_forgotten")?;
    let saved = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (path, hash) in saved {
        let relative = Path::new(&path);
        let file = root.join(relative);
        if relative
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
            && file
                .canonicalize()
                .is_ok_and(|resolved| resolved.starts_with(root))
            && fs::read(&file).is_ok_and(|bytes| hex::encode(Sha256::digest(bytes)) == hash)
        {
            omitted.insert(path);
        } else {
            db.execute("DELETE FROM rust_forgotten WHERE relative_path=?", [path])?;
        }
    }
    Ok(omitted)
}

fn matches(root: &Path, stored: &BTreeSet<String>, patterns: &[String]) -> Result<Vec<String>> {
    let mut selected = BTreeSet::new();
    for pattern in patterns.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let path = Path::new(pattern);
        let expanded = if let Ok(rest) = path.strip_prefix("~") {
            std::env::home_dir()
                .context("missing home directory")?
                .join(rest)
        } else {
            root.join(path)
        };
        let mut absolute = PathBuf::new();
        for part in expanded.components() {
            if part == Component::ParentDir {
                absolute.pop();
            } else if part != Component::CurDir {
                absolute.push(part);
            }
        }
        let glob = |s: &str| {
            GlobBuilder::new(s)
                .backslash_escape(false)
                .literal_separator(false)
                .build()
                .ok()
                .map(|g| g.compile_matcher())
        };
        let relative_pattern = glob(pattern);
        let absolute_pattern = absolute.to_str().and_then(glob);
        for name in stored {
            let candidate = Path::new(name);
            if candidate.starts_with(&absolute)
                || absolute_pattern
                    .as_ref()
                    .is_some_and(|g| g.is_match(candidate))
                || relative_pattern
                    .as_ref()
                    .is_some_and(|g| candidate.strip_prefix(root).is_ok_and(|p| g.is_match(p)))
            {
                selected.insert(name.clone());
            }
        }
    }
    Ok(selected.into_iter().collect())
}

pub(super) fn migrate_legacy(root: &Path, database: &Path) -> Result<()> {
    let old = root.join(".code-review-graph.db");
    if database != root.join(".code-review-graph/graph.db") || database.exists() || !old.is_file() {
        return Ok(());
    }
    let original = Connection::open_with_flags(&old, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    ensure!(
        metadata(&original, "schema_version")?.as_deref() == Some("15"),
        "unsupported graph schema"
    );
    ensure!(
        metadata(&original, "repo_root")?
            .as_deref()
            .is_none_or(|s| Path::new(s) == root),
        "index belongs to another repository"
    );
    let busy = original.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
        r.get::<_, i64>(0)
    })?;
    ensure!(busy == 0, "legacy graph has an active writer");
    drop(original);
    fs::rename(&old, database)?;
    Ok(())
}

pub fn run(
    root: &Path,
    database: &Path,
    patterns: &[String],
    preview: bool,
    wait: bool,
) -> Result<Outcome> {
    let old = root.join(".code-review-graph.db");
    let legacy = database == root.join(".code-review-graph/graph.db")
        && !database.is_file()
        && old.is_file();
    ensure!(
        database.is_file() || legacy,
        "No graph found at {}. Run `code-review-graph build` first.",
        database.display()
    );
    let _lock = if preview {
        None
    } else {
        super::ensure_writable(if legacy { &old } else { database })?;
        fs::create_dir_all(database.parent().context("missing graph directory")?)?;
        Some(WriteLock::acquire(
            database.parent().context("missing graph directory")?,
            wait,
        )?)
    };
    if !preview {
        migrate_legacy(root, database)?;
    }
    let mut db = Connection::open_with_flags(
        if preview && legacy { &old } else { database },
        if preview {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        },
    )?;
    let tx = db.transaction_with_behavior(if preview {
        TransactionBehavior::Deferred
    } else {
        TransactionBehavior::Immediate
    })?;
    let stored = tx.prepare("SELECT file_path FROM nodes WHERE extra IS NULL OR NOT json_valid(extra) OR coalesce(json_extract(extra,'$.virtual'),0)!=1 UNION SELECT file_path FROM edges ORDER BY file_path")?
        .query_map([],|r|r.get(0))?.collect::<rusqlite::Result<BTreeSet<String>>>()?;
    ensure!(
        metadata(&tx, "repo_root")?
            .as_deref()
            .is_none_or(|s| Path::new(s) == root)
            && stored.iter().all(|s| Path::new(s).starts_with(root)),
        "index belongs to another repository"
    );
    let selected = matches(root, &stored, patterns)?;
    let mut report = Outcome {
        tracked: stored.len(),
        files: selected,
        reparsed: 0,
        derived: None,
    };
    if report.files.is_empty() || preview {
        return Ok(report);
    }
    let foreign = metadata(&tx, "schema_version")?;
    ensure!(foreign.as_deref() == Some("15"), "unsupported graph schema");
    begin_edges(&tx)?;
    tx.execute_batch("CREATE TEMP TABLE forgotten(file TEXT PRIMARY KEY);
        CREATE TABLE IF NOT EXISTS rust_forgotten(relative_path TEXT PRIMARY KEY,sha256 TEXT NOT NULL);")?;
    for target in &report.files {
        tx.execute("INSERT INTO forgotten VALUES(?)", [target])?;
        let relative = Path::new(target)
            .strip_prefix(root)?
            .to_str()
            .context("non UTF-8 path")?;
        tx.execute("INSERT OR REPLACE INTO rust_forgotten SELECT ?1,file_hash FROM nodes WHERE kind='File' AND file_path=?2 AND file_hash IS NOT NULL",params![relative,target])?;
    }
    let referrers = tx.prepare("WITH vanished AS (SELECT qualified_name FROM nodes JOIN forgotten ON nodes.file_path=forgotten.file)
        SELECT file_path FROM edges WHERE source_qualified IN vanished OR target_qualified IN vanished EXCEPT SELECT file FROM forgotten ORDER BY file_path")?
        .query_map([],|r|r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
    tx.execute_batch("INSERT INTO affected SELECT file FROM forgotten; DELETE FROM nodes WHERE file_path IN forgotten")?;
    let omitted: BTreeSet<_> = report.files.iter().cloned().collect();
    let retained: BTreeSet<String> = stored
        .difference(&omitted)
        .map(|s| {
            Path::new(s)
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    if !referrers.is_empty() {
        let mut graph = Graph::open(Path::new(":memory:"))?;
        graph.sync_matching(root, true, |p| retained.contains(p))?;
        let staging = graph.connection();
        let writer = Writer {
            hcl: super::hcl::Modules::load(root, staging)?,
            root,
            staging,
            paths: &retained,
            ts_paths: crate::tsconfig::TsPaths::load(root, retained.iter()),
        };
        let returns = evidence::ReturnIndex::read(staging)?;
        for referrer in referrers {
            let abs = PathBuf::from(&referrer);
            if !abs.is_file() {
                continue;
            }
            let path = abs.strip_prefix(root)?;
            let relative = path.to_str().context("non UTF-8 path")?;
            let (raw, hash): (String, String) = staging
                .query_row(
                    "SELECT facts,hash FROM files WHERE path=?",
                    [relative],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .with_context(|| format!("unable to reparse referrer {relative}"))?;
            let bytes = fs::read(&abs)?;
            ensure!(
                blake3::hash(&bytes).to_hex().as_str() == hash,
                "source changed while indexing: {relative}"
            );
            let facts: ParsedFile = serde_json::from_str(&raw)?;
            let mut source = Source::parse(path, std::str::from_utf8(&bytes)?)?;
            if facts.cfamily.language == "cpp" {
                source.language = "cpp";
            }
            tx.execute("INSERT INTO affected VALUES(?)", [&referrer])?;
            tx.execute("DELETE FROM nodes WHERE file_path=?", [&referrer])?;
            writer.materialize_file(
                &tx,
                relative,
                &facts,
                &source,
                &hex::encode(Sha256::digest(bytes)),
                &returns,
            )?;
            report.reparsed += 1;
        }
    }
    replace_edges(&tx)?;
    set_metadata(&tx, "intentionally_incomplete", "1")?;
    report.derived = Some(postprocess::process(&tx, postprocess::Level::Full, false)?);
    let vectors = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='embeddings')",
        [],
        |r| r.get::<_, bool>(0),
    )?;
    if vectors {
        tx.execute(
            "DELETE FROM embeddings WHERE NOT EXISTS(SELECT 1 FROM nodes WHERE nodes.qualified_name=embeddings.qualified_name)",
            [],
        )?;
    }
    tx.commit()?;
    Ok(report)
}

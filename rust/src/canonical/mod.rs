mod docs;
pub mod postprocess;
mod source;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::{graph::Graph, model::ParsedFile};
use source::{Declaration, Source, import_target};

pub struct IndexReport {
    pub full: bool,
    pub files_parsed: usize,
    pub files_deleted: usize,
    pub nodes: i64,
    pub edges: i64,
    pub files: i64,
    pub postprocess: postprocess::Report,
}

struct WriteLock(File);

impl WriteLock {
    fn acquire(dir: &Path, wait: bool) -> Result<Self> {
        let mut file = File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("write.lock"))?;
        if wait {
            file.lock()?;
        } else {
            match file.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => {
                    anyhow::bail!("another writer is using this graph")
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        file.set_len(0)?;
        write!(file, "{}", std::process::id())?;
        Ok(Self(file))
    }
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        let _ = self.0.set_len(0);
    }
}

fn metadata(db: &Connection, key: &str) -> Result<Option<String>> {
    Ok(db
        .query_row("SELECT value FROM metadata WHERE key=?", [key], |r| {
            r.get(0)
        })
        .optional()?)
}

fn set_metadata(db: &Connection, key: &str, value: &str) -> Result<()> {
    db.execute(
        "INSERT INTO metadata VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        (key, value),
    )?;
    Ok(())
}

fn qualified(path: &str, name: &str) -> String {
    if name == "<module>" {
        path.to_owned()
    } else {
        format!("{path}::{name}")
    }
}

fn file_hashes(db: &Connection) -> Result<BTreeMap<String, String>> {
    Ok(db
        .prepare("SELECT file_path,file_hash FROM nodes WHERE kind='File'")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}

pub fn index(
    root: &Path,
    path: &Path,
    full: bool,
    level: postprocess::Level,
    wait: bool,
) -> Result<IndexReport> {
    let root = root.canonicalize()?;
    let dir = path.parent().context("missing graph directory")?;
    fs::create_dir_all(dir)?;
    let _lock = WriteLock::acquire(dir, wait)?;
    let mut db = Connection::open(path)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let empty = tx.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table')",
        [],
        |r| r.get::<_, bool>(0),
    )?;
    if empty {
        tx.execute_batch(include_str!("schema.sql"))?;
        set_metadata(&tx, "schema_version", "15")?;
        set_metadata(&tx, "rust_index_version", "1")?;
    } else {
        ensure!(
            metadata(&tx, "schema_version")?.as_deref() == Some("15"),
            "unsupported graph schema"
        );
        if metadata(&tx, "rust_index_version")?.is_none() {
            let other_language: Option<String> = tx
                .query_row(
                    "SELECT language FROM nodes WHERE kind='File' AND coalesce(language,'') NOT IN
                ('python','rust','javascript','typescript','tsx') LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            ensure!(
                other_language.is_none(),
                "this graph contains a language the native writer cannot update: {}",
                other_language.unwrap_or_default()
            );
            let has_embeddings = tx.query_row("SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE type='table' AND name='embeddings')", [], |r|r.get::<_,bool>(0))?;
            ensure!(
                !has_embeddings
                    || !tx.query_row("SELECT EXISTS(SELECT 1 FROM embeddings)", [], |r| r
                        .get::<_, bool>(0))?,
                "this graph contains embeddings that require an embedding-aware writer"
            );
        } else {
            ensure!(
                metadata(&tx, "rust_index_version")?.as_deref() == Some("1"),
                "unsupported native index version"
            );
        }
        let owner = metadata(&tx, "repo_root")?;
        ensure!(
            owner.as_deref() == root.to_str(),
            "index belongs to another repository"
        );
    }

    let full = full
        || empty
        || (root.join(".git").exists()
            && !metadata(&tx, "git_head_sha")?.is_some_and(|sha| {
                git(
                    &root,
                    &[
                        "rev-parse",
                        "--verify",
                        "--end-of-options",
                        &format!("{sha}^{{commit}}"),
                    ],
                )
                .is_some()
            }));
    let mut graph = Graph::open(&path.with_extension("rust-cache"))?;
    graph.sync(&root, full)?;
    let staging = graph.connection();
    let files: BTreeMap<String, (String, String)> = staging
        .prepare("SELECT path,hash,facts FROM files ORDER BY path")?
        .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
        .collect::<rusqlite::Result<_>>()?;
    let paths: BTreeSet<_> = files.keys().cloned().collect();
    let writer = Writer {
        staging,
        root: &root,
        paths: &paths,
    };
    let mut manifest = blake3::Hasher::new();
    for (path, (hash, _)) in &files {
        manifest
            .update(path.as_bytes())
            .update(&[0])
            .update(hash.as_bytes());
    }
    let manifest = manifest.finalize().to_hex().to_string();
    let mut parsed = 0;
    let mut deleted = 0;
    let mut changed = false;
    if full || metadata(&tx, "rust_source_manifest")?.as_deref() != Some(&manifest) {
        (parsed, deleted) = writer.apply(&tx, &files, full)?;
        changed = full || parsed != 0 || deleted != 0;
        set_metadata(&tx, "rust_source_manifest", &manifest)?;
        set_metadata(&tx, "rust_index_version", "1")?;
    }
    if changed {
        tx.execute("INSERT OR REPLACE INTO metadata VALUES('last_updated',strftime('%Y-%m-%dT%H:%M:%S','now','localtime'))",[])?;
        for (key, value) in [
            ("repo_root", root.to_str().context("non UTF-8 root")?),
            ("last_build_type", if full { "full" } else { "incremental" }),
            ("intentionally_incomplete", "0"),
            ("build_state", "postprocess-pending"),
            ("fts_state_synced", "2"),
        ] {
            set_metadata(&tx, key, value)?;
        }
        set_metadata(&tx, "verified_file_count", &files.len().to_string())?;
    }
    if let Some(head) = git(&root, &["rev-parse", "--verify", "HEAD"]) {
        set_metadata(&tx, "git_head_sha", &head)?;
        if let Some(branch) = git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]) {
            set_metadata(&tx, "git_branch", &branch)?;
        }
    }
    let postprocess = if changed
        || (level != postprocess::Level::None
            && (metadata(&tx, "build_state")?.as_deref() != Some("complete")
                || (level == postprocess::Level::Full
                    && metadata(&tx, "postprocess_level")?.as_deref() != Some("full"))))
    {
        postprocess::process(&tx, level, full)?
    } else {
        postprocess::Report::default()
    };
    let report = IndexReport {
        postprocess,
        full,
        files_parsed: parsed,
        files_deleted: deleted,
        files: files.len() as i64,
        nodes: tx.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?,
        edges: tx.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?,
    };
    tx.commit()?;
    Ok(report)
}

struct NodeRow<'a> {
    name: &'a str,
    qualified: &'a str,
    kind: &'a str,
    parent: Option<&'a str>,
    start: usize,
    end: usize,
    declaration: Option<&'a Declaration>,
}

fn write_node(
    tx: &Transaction<'_>,
    path: &str,
    language: &str,
    hash: &str,
    node: NodeRow<'_>,
) -> Result<()> {
    let fallback = Declaration::default();
    let declaration = node.declaration.unwrap_or(&fallback);
    let doc = declaration.doc.as_deref();
    let mut extra = serde_json::Map::new();
    if !declaration.decorators.is_empty() {
        extra.insert(
            "decorators".into(),
            serde_json::json!(declaration.decorators),
        );
    }
    if let Some(doc) = doc {
        extra.insert("docstring".into(), doc.into());
    }
    let extra = serde_json::Value::Object(extra).to_string();
    tx.prepare_cached("INSERT INTO nodes(kind,name,qualified_name,file_path,line_start,line_end,language,
        parent_name,params,return_type,modifiers,is_test,file_hash,extra,symbol,docstring,name_tokens,updated_at)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,unixepoch('subsec'))
        ON CONFLICT(qualified_name) DO UPDATE SET kind=excluded.kind,name=excluded.name,line_start=excluded.line_start,
        line_end=excluded.line_end,parent_name=excluded.parent_name,params=excluded.params,return_type=excluded.return_type,
        modifiers=excluded.modifiers,is_test=excluded.is_test,file_hash=excluded.file_hash,extra=excluded.extra,
        symbol=excluded.symbol,docstring=excluded.docstring,name_tokens=excluded.name_tokens,updated_at=excluded.updated_at
        WHERE nodes.file_hash!=excluded.file_hash")?
        .execute(params![node.kind,node.name,node.qualified,path,node.start as i64,node.end as i64,language,node.parent,
            declaration.params,declaration.returns,declaration.modifiers,node.kind=="Test",hash,extra,
            node.qualified.split_once("::").map_or(node.qualified,|(_,s)|s.split_once(":S").map_or(s, |(base,_)| base)),doc,index_tokens(node.name,node.kind,path,node.parent)])?;
    Ok(())
}

fn index_tokens(name: &str, kind: &str, path: &str, parent: Option<&str>) -> String {
    let stem = Path::new(path)
        .file_stem()
        .and_then(|p| p.to_str())
        .unwrap_or("");
    let mut tokens = Vec::new();
    for value in [
        if kind == "File" { "" } else { name },
        parent.unwrap_or(""),
        stem,
    ] {
        for word in value
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|s| !s.is_empty())
        {
            let b = word.as_bytes();
            let cuts: Vec<_> = (1..b.len())
                .filter(|&i| {
                    b[i].is_ascii_uppercase()
                        && (b[i - 1].is_ascii_lowercase()
                            || b[i - 1].is_ascii_digit()
                            || (b[i - 1].is_ascii_uppercase()
                                && b.get(i + 1).is_some_and(u8::is_ascii_lowercase)))
                })
                .collect();
            if cuts.is_empty() {
                continue;
            }
            let mut start = 0;
            for end in cuts.into_iter().chain(std::iter::once(b.len())) {
                let part = &word[start..end];
                if !tokens.iter().any(|p: &&str| p.eq_ignore_ascii_case(part)) {
                    tokens.push(part);
                }
                start = end;
            }
        }
    }
    tokens.join(" ").chars().take(200).collect()
}

fn edge(
    tx: &Transaction<'_>,
    kind: &str,
    source: &str,
    target: &str,
    path: &str,
    line: usize,
    resolution: Option<&str>,
) -> Result<()> {
    tx.prepare_cached("INSERT OR IGNORE INTO imported(kind,source,target,path,line,resolution) VALUES(?,?,?,?,?,?)")?
        .execute(params![kind, source, target, path, line as i64, resolution])?;
    Ok(())
}

struct Writer<'a> {
    staging: &'a Connection,
    root: &'a Path,
    paths: &'a BTreeSet<String>,
}

impl Writer<'_> {
    fn apply(
        &self,
        tx: &Transaction<'_>,
        files: &BTreeMap<String, (String, String)>,
        full: bool,
    ) -> Result<(usize, usize)> {
        tx.execute_batch("CREATE TEMP TABLE affected(file TEXT PRIMARY KEY);
            CREATE TEMP TABLE imported(kind TEXT,source TEXT,target TEXT,path TEXT,line INTEGER,resolution TEXT,extra TEXT DEFAULT '{}',
                PRIMARY KEY(kind,source,target,path,line))")?;
        let mut old = file_hashes(tx)?;
        let mut digests = BTreeMap::new();
        for (relative, (hash, _)) in files {
            let absolute = self.root.join(relative);
            let path = absolute.to_str().context("non UTF-8 repository path")?;
            let data = fs::read(&absolute)?;
            ensure!(
                blake3::hash(&data).to_hex().as_str() == hash,
                "source changed while indexing: {relative}"
            );
            let sha = hex::encode(Sha256::digest(&data));
            if old.remove(path).as_ref() != Some(&sha) || full {
                tx.execute("INSERT INTO affected VALUES(?)", [path])?;
                tx.execute("DELETE FROM nodes WHERE file_path=?", [path])?;
            }
            digests.insert(relative.as_str(), sha);
        }
        let deleted = old.len();
        for path in old.keys() {
            tx.execute("INSERT INTO affected VALUES(?)", [path])?;
            tx.execute("DELETE FROM nodes WHERE file_path=?", [path])?;
        }
        if !full {
            tx.execute_batch("WITH RECURSIVE deps(file) AS (
                SELECT file FROM affected UNION SELECT e.file_path FROM edges e JOIN deps d ON e.target_qualified=d.file
                   WHERE e.kind='IMPORTS_FROM'
            ) INSERT OR IGNORE INTO affected SELECT file FROM deps")?;
        }
        let affected: Vec<String> = tx
            .prepare("SELECT file FROM affected ORDER BY file")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if affected.is_empty() {
            return Ok((0, deleted));
        }
        for absolute in &affected {
            let relative = Path::new(absolute)
                .strip_prefix(self.root)?
                .to_str()
                .context("non UTF-8 repository path")?;
            let Some((hash, facts)) = files.get(relative) else {
                continue;
            };
            let data = fs::read(absolute)?;
            ensure!(
                blake3::hash(&data).to_hex().as_str() == hash,
                "source changed while indexing: {relative}"
            );
            let source = Source::parse(Path::new(relative), std::str::from_utf8(&data)?)?;
            self.materialize_file(
                tx,
                relative,
                &serde_json::from_str(facts)?,
                &source,
                &digests[relative],
            )?;
        }
        tx.execute_batch("DELETE FROM edges WHERE file_path IN (SELECT file FROM affected);
            INSERT OR IGNORE INTO imported(kind,source,target,path,line,extra)
                SELECT 'TESTED_BY',e.target,e.source,e.path,e.line,e.extra FROM imported e
                JOIN nodes n ON n.qualified_name=e.source WHERE e.kind='CALLS' AND n.is_test=1;
            INSERT INTO edges(kind,source_qualified,target_qualified,file_path,line,target_resolution,extra,updated_at)
                SELECT kind,source,target,path,line,resolution,extra,unixepoch('subsec') FROM imported ORDER BY rowid")?;
        Ok((if full { files.len() } else { affected.len() }, deleted))
    }

    fn materialize_file(
        &self,
        tx: &Transaction<'_>,
        relative: &str,
        facts: &ParsedFile,
        source: &Source,
        sha: &str,
    ) -> Result<()> {
        let Self {
            staging,
            root,
            paths,
        } = self;
        let full_path = root.join(relative);
        let path = full_path.to_str().context("non UTF-8 path")?;
        write_node(
            tx,
            path,
            source.language,
            sha,
            NodeRow {
                name: path,
                qualified: path,
                kind: "File",
                parent: None,
                start: 1,
                end: source.lines,
                declaration: None,
            },
        )?;
        for symbol in &facts.symbols {
            let qn = qualified(path, &symbol.name);
            let identity = symbol
                .name
                .split_once(":S")
                .map_or(symbol.name.as_str(), |(s, _)| s);
            let (parent, name) = identity
                .rsplit_once('.')
                .map_or((None, identity), |(p, n)| (Some(p), n));
            let details = source
                .declarations
                .get(&(symbol.start_line, name.to_owned()));
            let python = facts.python.definitions.get(&symbol.name);
            let enriched = python.map(|d| Declaration {
                params: d.params.clone(),
                returns: d.returns.clone(),
                modifiers: d.modifiers.clone(),
                decorators: d.decorators.clone(),
                doc: details.and_then(|s| s.doc.clone()),
            });
            write_node(
                tx,
                path,
                source.language,
                sha,
                NodeRow {
                    name,
                    qualified: &qn,
                    kind: &symbol.kind,
                    parent,
                    start: symbol.start_line,
                    end: symbol.end_line,
                    declaration: enriched.as_ref().or(details),
                },
            )?;
            let container = parent
                .filter(|_| !(source.language == "python" && symbol.kind == "Class"))
                .map(|p| qualified(path, p))
                .unwrap_or_else(|| path.to_owned());
            edge(
                tx,
                "CONTAINS",
                &container,
                &qn,
                path,
                symbol.start_line,
                None,
            )?;
            if let Some(def) = python
                && symbol.kind == "Class"
            {
                for base in &def.bases {
                    edge(tx, "INHERITS", &qn, base, path, symbol.start_line, None)?;
                }
            }
        }
        for (line, module) in &source.imports {
            let target = import_target(relative, module, source.language, paths)
                .map(|p| root.join(p).to_string_lossy().into_owned())
                .unwrap_or_else(|| module.clone());
            edge(tx, "IMPORTS_FROM", path, &target, path, *line, None)?;
        }
        for (kind, from, to, line) in source.extra_edges.rows(root, relative, facts, paths) {
            edge(tx, kind, &from, &to, path, line, None)?;
        }
        let mut resolved = BTreeSet::new();
        let mut stmt = staging.prepare_cached(
            "SELECT source,raw_target,line,target_file,target FROM call_sites WHERE source_file=?",
        )?;
        let mut rows = stmt.query([relative])?;
        while let Some(row) = rows.next()? {
            let caller: String = row.get(0)?;
            let raw: String = row.get(1)?;
            let line = usize::try_from(row.get::<_, i64>(2)?)?;
            let file: String = row.get(3)?;
            let symbol: String = row.get(4)?;
            let target = qualified(&root.join(file).to_string_lossy(), &symbol);
            edge(
                tx,
                "CALLS",
                &qualified(path, &caller),
                &target,
                path,
                line,
                Some("direct"),
            )?;
            resolved.insert((caller, raw, line));
        }
        for call in &facts.calls {
            if !resolved.contains(&(call.caller.clone(), call.callee.clone(), call.line)) {
                let claim = source.claimed_call(root, relative, &call.callee, facts, paths);
                let bare = claim
                    .as_ref()
                    .map(|(name, _)| name.as_str())
                    .unwrap_or_else(|| call.callee.rsplit('.').next().unwrap_or(&call.callee));
                edge(
                    tx,
                    "CALLS",
                    &qualified(path, &call.caller),
                    bare,
                    path,
                    call.line,
                    Some("unresolved"),
                )?;
                let extra = if let Some((_, target)) = claim.as_ref() {
                    Some(serde_json::json!({"claimed_endpoint":target}))
                } else {
                    call.callee.rsplit_once('.').map(|(receiver,_)| if receiver == "<expression>" {
                        serde_json::json!({"receiver_form":"expression","receiver_binding":"unknown"})
                    } else {
                        serde_json::json!({"receiver":receiver,"receiver_binding":"unknown"})
                    })
                };
                if let Some(extra) = extra {
                    tx.execute("UPDATE imported SET extra=? WHERE kind='CALLS' AND source=? AND target=? AND path=? AND line=?",
                        params![extra.to_string(),qualified(path,&call.caller),bare,path,call.line as i64])?;
                }
            }
        }
        Ok(())
    }
}

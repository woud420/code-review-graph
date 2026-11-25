#[cfg(test)]
mod cache_upgrade;
mod docs;
mod evidence;
mod imports;
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
use imports::import_target;
use source::{Declaration, Source};

pub struct IndexReport {
    pub base: Option<String>,
    pub interrupted: bool,
    pub changed_files: Vec<String>,
    pub dependent_files: Vec<String>,
    pub effective_level: postprocess::Level,
    pub full: bool,
    pub files_parsed: usize,
    pub files_deleted: usize,
    pub nodes: i64,
    pub edges: i64,
    pub files: i64,
    pub postprocess: postprocess::Report,
}

const INDEX_VERSION: &str = "2";

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
    let mut upgrade = false;
    if empty {
        tx.execute_batch(include_str!("schema.sql"))?;
        set_metadata(&tx, "schema_version", "15")?;
        set_metadata(&tx, "rust_index_version", INDEX_VERSION)?;
    } else {
        ensure!(
            metadata(&tx, "schema_version")?.as_deref() == Some("15"),
            "unsupported graph schema"
        );
        if metadata(&tx, "rust_index_version")?.is_none() {
            let other_language: Option<String> = tx
                .query_row(
                    "SELECT language FROM nodes WHERE kind='File' AND coalesce(language,'') NOT IN
                ('python','rust','javascript','typescript','tsx','bash','json') LIMIT 1",
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
            match metadata(&tx, "rust_index_version")?.as_deref() {
                Some("1") => upgrade = true,
                Some(INDEX_VERSION) => {}
                _ => anyhow::bail!("unsupported native index version"),
            }
        }
        let owner = metadata(&tx, "repo_root")?;
        ensure!(
            owner.as_deref() == root.to_str(),
            "index belongs to another repository"
        );
    }

    let previous_state = metadata(&tx, "build_state")?;
    let interrupted = matches!(
        previous_state.as_deref(),
        Some("in-progress" | "postprocess-pending")
    );
    let base = if root.join(".git").exists() {
        metadata(&tx, "git_head_sha")?
    } else {
        Some("HEAD~1".to_owned())
    };
    let full = full
        || upgrade
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
    let ts_paths = crate::tsconfig::TsPaths::load(&root, paths.iter());
    let writer = Writer {
        ts_paths,
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
    let mut changed_files = Vec::new();
    let mut dependent_files = Vec::new();
    let mut parsed = 0;
    let mut deleted = 0;
    let mut changed = false;
    if full || metadata(&tx, "rust_source_manifest")?.as_deref() != Some(&manifest) {
        (parsed, deleted) = writer.apply(&tx, &files, full)?;
        if !full {
            for (output, sql) in [
                (
                    &mut changed_files,
                    "SELECT file FROM original_changes ORDER BY file",
                ),
                (
                    &mut dependent_files,
                    "SELECT file FROM affected EXCEPT SELECT file FROM original_changes ORDER BY file",
                ),
            ] {
                let mut stmt = tx.prepare(sql)?;
                for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
                    output.push(
                        Path::new(&row?)
                            .strip_prefix(&root)?
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
        changed = full || parsed != 0 || deleted != 0;
        set_metadata(&tx, "rust_source_manifest", &manifest)?;
        set_metadata(&tx, "rust_index_version", INDEX_VERSION)?;
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
    let effective_level = if !changed && !interrupted && level == postprocess::Level::Full {
        postprocess::Level::Minimal
    } else {
        level
    };
    let postprocess = if changed || effective_level != postprocess::Level::None {
        postprocess::process(&tx, effective_level, full)?
    } else {
        postprocess::Report::default()
    };
    let report = IndexReport {
        base: if full { None } else { base },
        interrupted,
        changed_files,
        dependent_files,
        effective_level,
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
    test_file: bool,
    name: &'a str,
    qualified: &'a str,
    kind: &'a str,
    parent: Option<&'a str>,
    start: usize,
    end: usize,
    declaration: Option<&'a Declaration>,
    extra: Option<&'a serde_json::Value>,
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
    let mut extra = node
        .extra
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
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
            declaration.params,declaration.returns,declaration.modifiers,node.test_file || node.kind=="Test",hash,extra,
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

fn import_edge(
    tx: &Transaction<'_>,
    path: &str,
    target: &str,
    line: usize,
    extra: &serde_json::Value,
) -> Result<()> {
    tx.prepare_cached("INSERT OR IGNORE INTO imported(kind,source,target,path,line,extra) VALUES('IMPORTS_FROM',?1,?2,?1,?3,?4)")?
        .execute(params![path,target,line as i64,extra.to_string()])?;
    Ok(())
}

struct Writer<'a> {
    ts_paths: crate::tsconfig::TsPaths,
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
            tx.execute_batch("CREATE TEMP TABLE original_changes AS SELECT file FROM affected; WITH RECURSIVE deps(file) AS (
                SELECT file FROM affected UNION SELECT e.file_path FROM edges e JOIN deps d ON e.target_qualified=d.file
                   WHERE e.kind='IMPORTS_FROM'
            ) INSERT OR IGNORE INTO affected SELECT file FROM deps")?;
        }
        if !full {
            tx.execute_batch("INSERT OR IGNORE INTO affected
                SELECT file_path FROM nodes WHERE kind='File' AND language IN ('typescript','tsx','javascript')
                AND EXISTS(SELECT 1 FROM original_changes WHERE file LIKE '%/tsconfig.json' OR file LIKE '%/jsconfig.json')")?;
        }
        let affected: Vec<String> = tx
            .prepare("SELECT file FROM affected ORDER BY file")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if affected.is_empty() {
            return Ok((0, deleted));
        }
        let returns = evidence::ReturnIndex::read(self.staging)?;
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
            let facts: ParsedFile = serde_json::from_str(facts)?;
            let mut source = Source::parse(Path::new(relative), std::str::from_utf8(&data)?)?;
            if facts.cfamily.language == "cpp" {
                source.language = "cpp";
            }
            self.materialize_file(tx, relative, &facts, &source, &digests[relative], &returns)?;
        }
        tx.execute_batch("DELETE FROM edges WHERE file_path IN (SELECT file FROM affected);
            INSERT OR IGNORE INTO imported(kind,source,target,path,line,extra)
                SELECT 'TESTED_BY',e.target,e.source,e.path,e.line,e.extra FROM imported e
                JOIN nodes n ON n.qualified_name=e.source WHERE e.kind='CALLS' AND n.kind='Test';
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
        returns: &evidence::ReturnIndex,
    ) -> Result<()> {
        let Self {
            staging,
            root,
            paths,
            ts_paths,
        } = self;
        let test_file = crate::model::test_file(relative, root.to_str());
        let full_path = root.join(relative);
        let path = full_path.to_str().context("non UTF-8 path")?;
        write_node(
            tx,
            path,
            source.language,
            sha,
            NodeRow {
                test_file,
                name: path,
                qualified: path,
                kind: "File",
                parent: None,
                start: 1,
                end: source.lines,
                declaration: None,
                extra: Some(&facts.json.file),
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
            let json = facts.json.paths.get(&symbol.name);
            let (parent, name) = if json.is_some() {
                (None, symbol.name.strip_prefix("json:").unwrap_or(name))
            } else {
                (parent, name)
            };
            let cc = facts
                .cfamily
                .declarations
                .iter()
                .find(|d| d.identity == symbol.name && d.start == symbol.start_line);
            let (parent, name) =
                cc.map_or((parent, name), |d| (d.parent.as_deref(), d.name.as_str()));
            let c_decl = cc.map(|d| Declaration {
                params: d.params.clone(),
                doc: d.doc.clone(),
                ..Default::default()
            });
            let details = source
                .declarations
                .get(&(symbol.start_line, name.to_owned()));
            let python = facts.python.definitions.get(&symbol.name);
            let go = facts.go.declarations.get(&symbol.name);
            let go_declaration = go.map(|d| Declaration {
                params: d.params.clone(),
                doc: d.doc.clone(),
                ..Default::default()
            });
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
                    test_file,
                    name,
                    qualified: &qn,
                    kind: &symbol.kind,
                    parent,
                    start: symbol.start_line,
                    end: symbol.end_line,
                    declaration: c_decl
                        .as_ref()
                        .or(go_declaration.as_ref())
                        .or(enriched.as_ref())
                        .or(details),
                    extra: go
                        .map(|d| &d.extra)
                        .or_else(|| json.map(|p| &p.extra))
                        .or_else(|| facts.typescript.kinds.get(&symbol.name)),
                },
            )?;
            let container = if let Some(c) = cc {
                c.container.as_deref()
            } else {
                json.and_then(|p| p.parent.as_deref())
                    .or_else(|| {
                        facts
                            .typescript
                            .containers
                            .get(&symbol.name)
                            .map(String::as_str)
                    })
                    .or(parent)
            }
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
            if let Some(d) = cc {
                for base in &d.bases {
                    edge(tx, "INHERITS", &qn, base, path, symbol.start_line, None)?;
                }
            }
            if let Some(bases) = facts.typescript.bases.get(&symbol.name) {
                for base in bases {
                    edge(tx, "INHERITS", &qn, base, path, symbol.start_line, None)?;
                }
            }
            if let Some(d) = go {
                for base in &d.bases {
                    edge(tx, "INHERITS", &qn, base, path, symbol.start_line, None)?;
                }
            }
            if let Some(def) = python
                && symbol.kind == "Class"
            {
                for base in &def.bases {
                    edge(tx, "INHERITS", &qn, base, path, symbol.start_line, None)?;
                }
            }
        }
        for (line, module) in &facts.bash_imports {
            let full = root.join(relative);
            let path_from_caller = full.parent().unwrap_or(root).join(module);
            let target = path_from_caller.canonicalize().ok().filter(|p| p.is_file());
            import_edge(
                tx,
                path,
                target.as_deref().and_then(Path::to_str).unwrap_or(module),
                *line,
                &serde_json::json!({}),
            )?;
        }
        for (line, module) in &facts.cfamily.imports {
            edge(tx, "IMPORTS_FROM", path, module, path, *line, None)?;
        }
        for (line, module) in &facts.go.imports {
            let target = crate::parser::go::package_dir(root, relative, module, paths);
            let extra = if target.is_some() {
                serde_json::json!({"import_scope":"package"})
            } else {
                serde_json::json!({})
            };
            import_edge(tx, path, target.as_deref().unwrap_or(module), *line, &extra)?;
        }
        for (caller, name, line) in &facts.typescript.references {
            let target = if facts
                .symbols
                .iter()
                .any(|s| s.name == *name && !s.name.contains('.'))
            {
                qualified(path, name)
            } else if let Some(binding) = facts.imports.iter().find(|b| b.local == *name) {
                import_target(relative, &binding.module, source.language, paths)
                    .or_else(|| ts_paths.resolve(relative, &binding.module, |p| paths.contains(p)))
                    .map(|f| qualified(&root.join(f).to_string_lossy(), &binding.imported))
                    .unwrap_or_else(|| name.clone())
            } else {
                continue;
            };
            edge(
                tx,
                "REFERENCES",
                &qualified(path, caller),
                &target,
                path,
                *line,
                None,
            )?;
        }
        for (line, module) in &source.imports {
            let target = import_target(relative, module, source.language, paths)
                .or_else(|| ts_paths.resolve(relative, module, |p| paths.contains(p)))
                .map(|p| root.join(p).to_string_lossy().into_owned())
                .unwrap_or_else(|| module.clone());
            let (target, extra) = source
                .import_details
                .resolve(root, relative, module, target, *line, paths);
            import_edge(tx, path, &target, *line, &extra)?;
        }
        for (kind, from, to, line) in source.extra_edges.rows(root, relative, facts, paths) {
            if kind == "IMPORTS_FROM" {
                let (target, extra) = source
                    .import_details
                    .resolve(root, relative, "", to, line, paths);
                import_edge(tx, &from, &target, line, &extra)?;
            } else {
                edge(tx, kind, &from, &to, path, line, None)?;
            }
        }
        let evidence = evidence::CallEvidence::new(root, relative, facts, source, paths, returns);
        let mut write_call=tx.prepare_cached("INSERT INTO imported(kind,source,target,path,line,resolution,extra) VALUES('CALLS',?,?,?,?,?,?)
            ON CONFLICT(kind,source,target,path,line) DO UPDATE SET extra=excluded.extra")?;
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
            let target = qualified(&root.join(&file).to_string_lossy(), &symbol);
            let extra = evidence.describe(&caller, &raw, line, Some((&file, &symbol)));
            write_call.execute(params![
                qualified(path, &caller),
                target,
                path,
                line as i64,
                "direct",
                if extra.is_null() {
                    "{}".into()
                } else {
                    extra.to_string()
                }
            ])?;
            resolved.insert((caller, raw, line));
        }
        for call in &facts.calls {
            if !resolved.contains(&(call.caller.clone(), call.callee.clone(), call.line)) {
                let claim = source.claimed_call(root, relative, &call.callee, facts, paths);
                let bare = claim
                    .as_ref()
                    .map(|(name, _)| name.as_str())
                    .unwrap_or_else(|| call.callee.rsplit('.').next().unwrap_or(&call.callee));
                let mut extra = evidence.describe(&call.caller, &call.callee, call.line, None);
                if let Some((_, target)) = &claim {
                    extra["claimed_endpoint"] = serde_json::json!(target);
                }
                write_call.execute(params![
                    qualified(path, &call.caller),
                    bare,
                    path,
                    call.line as i64,
                    "unresolved",
                    if extra.is_null() {
                        "{}".into()
                    } else {
                        extra.to_string()
                    }
                ])?;
            }
        }
        Ok(())
    }
}

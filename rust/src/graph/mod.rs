mod csharp;
mod csharp_index;
mod php;
use crate::model::{BuildReport, Hit, Import, ParsedFile, Stats};
use crate::parser::parse_file;
use anyhow::{Context, Result, ensure};
use ignore::WalkBuilder;
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const APPLICATION_ID: i64 = 0x43524752; // "CRGR", distinct from the Python database.
const SCHEMA_VERSION: i64 = 3;
const CALL_SITES: &str = "CREATE TABLE call_sites (
    source_file TEXT NOT NULL, source TEXT NOT NULL, raw_target TEXT NOT NULL,
    target_file TEXT NOT NULL, target TEXT NOT NULL, line INTEGER NOT NULL,
    PRIMARY KEY(source_file,source,raw_target,target_file,target,line))";
const MODULE: &str = "<module>";
const JS_EXTENSIONS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

const SCHEMA: &str = "
CREATE TABLE metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE files(path TEXT PRIMARY KEY, hash TEXT NOT NULL, facts TEXT NOT NULL);
CREATE TABLE symbols(
    file TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    name TEXT NOT NULL, kind TEXT NOT NULL, start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
    PRIMARY KEY(file, name, start_line)
);
CREATE INDEX symbols_by_name ON symbols(name);
CREATE TABLE calls(
    source_file TEXT NOT NULL, source TEXT NOT NULL, target_file TEXT NOT NULL, target TEXT NOT NULL,
    line INTEGER NOT NULL, PRIMARY KEY(source_file, source, target_file, target, line)
);
CREATE INDEX calls_by_target ON calls(target_file, target);";

pub struct Graph {
    conn: Connection,
}

fn validate(conn: &Connection) -> Result<()> {
    let app: i64 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    ensure!(
        app == APPLICATION_ID,
        "this is not a code-review-graph-rs database"
    );
    ensure!(
        (1..=SCHEMA_VERSION).contains(&version),
        "unsupported Rust graph schema {version}"
    );
    Ok(())
}

impl Graph {
    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(15))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let tables: i64 = tx.query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table'",
                [],
                |r| r.get(0),
            )?;
            if tables == 0 {
                tx.execute_batch(SCHEMA)?;
                tx.execute_batch(CALL_SITES)?;
                tx.pragma_update(None, "application_id", APPLICATION_ID)?;
                tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            validate(&tx)?;
            tx.commit()?;
        }
        Ok(Self { conn })
    }

    pub fn open_existing(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("opening {} for reading", path.display()))?;
        conn.busy_timeout(Duration::from_secs(15))?;
        validate(&conn)?;
        let built: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM metadata WHERE key='root')",
            [],
            |row| row.get(0),
        )?;
        ensure!(built, "this graph has not completed its first build");
        Ok(Self { conn })
    }

    pub fn stats(&self) -> Result<Stats> {
        stats(&self.conn)
    }

    pub fn sync(&mut self, root: &Path, rebuild: bool) -> Result<BuildReport> {
        self.sync_matching(root, rebuild, |_| true)
    }

    pub(crate) fn sync_matching(
        &mut self,
        root: &Path,
        rebuild: bool,
        include: impl Fn(&str) -> bool,
    ) -> Result<BuildReport> {
        let root = fs::canonicalize(root).with_context(|| format!("opening {}", root.display()))?;
        ensure!(root.is_dir(), "repository path must be a directory");
        let root_name = root.to_str().context("repository path is not UTF-8")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous_root: Option<String> = tx
            .query_row("SELECT value FROM metadata WHERE key='root'", [], |r| {
                r.get(0)
            })
            .optional()?;
        ensure!(
            rebuild || previous_root.as_deref().is_none_or(|s| s == root_name),
            "this graph belongs to a different repository; run build to replace it"
        );
        let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version == 1 {
            tx.execute_batch(CALL_SITES)?;
        }
        let rebuild = rebuild || version != SCHEMA_VERSION;
        tx.execute(
            "INSERT INTO metadata VALUES('root', ?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [root_name],
        )?;

        let mut old = {
            let mut query = tx.prepare("SELECT path,hash FROM files")?;
            let rows = query.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<HashMap<String, String>>>()?
        };
        let mut files_parsed = 0;
        let mut files_unchanged = 0;
        let mut dbt = crate::parser::sql::dbt::Projects::default();
        for file in source_files(&root)?.into_iter().filter(|p| include(p)) {
            let bytes = fs::read(root.join(&file)).with_context(|| format!("reading {file}"))?;
            let hash = blake3::hash(&bytes).to_hex().to_string();
            let previous_hash = old.remove(&file);
            let path = Path::new(&file);
            let sql = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(crate::parser::sql::supports);
            let membership = sql.then(|| dbt.membership(&root, path)).flatten();
            if !rebuild && previous_hash.as_deref() == Some(&hash) {
                let same_context = !sql || tx.query_row(
                    "SELECT json_extract(facts,'$.sql.dbt_membership') IS ? FROM files WHERE path=?",
                    params![membership, file], |r| r.get::<_, bool>(0)
                )?;
                if same_context {
                    files_unchanged += 1;
                    continue;
                }
            }
            let source =
                std::str::from_utf8(&bytes).with_context(|| format!("{file} is not UTF-8"))?;
            let facts = (if sql {
                crate::parser::sql::parse(path, source, membership)
            } else {
                parse_file(path, source)
            })
            .with_context(|| format!("parsing {file}"))?;
            store_file(&tx, &file, &hash, &facts, source.lines().count())?;
            files_parsed += 1;
        }
        let files_deleted = old.len();
        for path in old.keys() {
            tx.execute("DELETE FROM files WHERE path=?", [path])?;
        }
        if rebuild || files_parsed > 0 || files_deleted > 0 {
            resolve_graph(&tx)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        let Stats {
            files,
            symbols,
            edges,
        } = stats(&tx)?;
        tx.commit()?;
        Ok(BuildReport {
            files_parsed,
            files_deleted,
            files_unchanged,
            files,
            symbols,
            edges,
        })
    }

    pub fn symbols(&self, query: &str) -> Result<Vec<Hit>> {
        let sql = "SELECT file,name,kind,start_line,end_line FROM symbols
                   WHERE instr(lower(name),lower(?1))>0 OR instr(lower(file||'::'||name),lower(?1))>0
                   ORDER BY file,start_line,name";
        let mut stmt = self.conn.prepare(sql)?;
        let results = stmt
            .query_map([query], row_hit)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(results)
    }

    fn seeds(&self, query: &str) -> Result<Vec<Hit>> {
        let sql = "SELECT file,name,kind,start_line,end_line FROM symbols
                   WHERE file||'::'||name=?1 OR file=?1 OR name=?1
                   ORDER BY file,start_line,name";
        let mut stmt = self.conn.prepare(sql)?;
        let results = stmt
            .query_map([query], row_hit)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(results)
    }

    fn neighbors(&self, file: &str, name: &str, incoming: bool) -> Result<Vec<Hit>> {
        let sql = if incoming {
            "SELECT DISTINCT s.file,s.name,s.kind,s.start_line,s.end_line
             FROM calls c JOIN symbols s ON s.file=c.source_file AND s.name=c.source
             WHERE c.target_file=? AND c.target=?"
        } else {
            "SELECT DISTINCT s.file,s.name,s.kind,s.start_line,s.end_line
             FROM calls c JOIN symbols s ON s.file=c.target_file AND s.name=c.target
             WHERE c.source_file=? AND c.source=?"
        };
        let mut stmt = self.conn.prepare_cached(sql)?;
        let results = stmt
            .query_map([file, name], row_hit)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(results)
    }

    fn adjacent(&self, query: &str, incoming: bool) -> Result<Vec<Hit>> {
        let snapshot = self.conn.unchecked_transaction()?;
        let mut results = BTreeSet::new();
        for hit in self.seeds(query)? {
            results.extend(self.neighbors(&hit.file, &hit.name, incoming)?);
        }
        snapshot.commit()?;
        Ok(results.into_iter().collect())
    }

    pub fn callers(&self, query: &str) -> Result<Vec<Hit>> {
        self.adjacent(query, true)
    }

    pub fn callees(&self, query: &str) -> Result<Vec<Hit>> {
        self.adjacent(query, false)
    }

    pub fn impact(&self, query: &str, depth: usize) -> Result<Vec<Hit>> {
        let snapshot = self.conn.unchecked_transaction()?;
        let mut frontier = self.seeds(query)?;
        let mut seen: BTreeSet<_> = frontier.iter().cloned().collect();
        let mut reached = Vec::new();
        for _ in 0..depth {
            let mut next = Vec::new();
            for hit in frontier {
                for caller in self.neighbors(&hit.file, &hit.name, true)? {
                    if seen.insert(caller.clone()) {
                        next.push(caller);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            next.sort();
            reached.extend(next.iter().cloned());
            frontier = next;
        }
        snapshot.commit()?;
        Ok(reached)
    }
}

fn row_hit(row: &rusqlite::Row<'_>) -> rusqlite::Result<Hit> {
    Ok(Hit {
        file: row.get(0)?,
        name: row.get(1)?,
        kind: row.get(2)?,
        start_line: row.get(3)?,
        end_line: row.get(4)?,
    })
}

fn stats(conn: &Connection) -> Result<Stats> {
    Ok(conn.query_row(
        "SELECT (SELECT count(*) FROM files), (SELECT count(*) FROM symbols),
        (SELECT count(*) FROM calls)",
        [],
        |r| {
            Ok(Stats {
                files: r.get(0)?,
                symbols: r.get(1)?,
                edges: r.get(2)?,
            })
        },
    )?)
}

fn source_files(root: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    let walk = WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|e| {
            e.depth() == 0
                || !matches!(
                    e.file_name().to_str(),
                    Some(
                        ".git"
                            | ".code-review-graph"
                            | ".code-review-graph-rs"
                            | "target"
                            | "node_modules"
                            | ".venv"
                            | "venv"
                            | "__pycache__"
                    )
                )
        })
        .build();
    for entry in walk {
        let entry = entry?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let json = ext == "json"
            && !matches!(
                file_name,
                "package-lock.json" | "npm-shrinkwrap.json" | "project.assets.json"
            )
            && !file_name.ends_with(".lock.json")
            && !file_name.ends_with(".min.json");
        if JS_EXTENSIONS.contains(&ext)
            || matches!(
                ext,
                "py" | "rs" | "go" | "java" | "cs" | "sol" | "lua" | "luau" | "php"
            )
            || crate::parser::extensions::language(ext).is_some()
            || json
            || crate::parser::cfamily::supports(ext)
            || crate::parser::bash::discover(path)
            || crate::parser::sql::supports(ext)
            || matches!(ext, "yaml" | "yml" | "tf" | "hcl" | "rb")
            || (ext == "properties" && crate::parser::yaml::spring::supports(path))
        {
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .context("source path is not UTF-8")?;
            files.push(relative.replace(std::path::MAIN_SEPARATOR, "/"));
        }
    }
    files.sort();
    Ok(files)
}

fn store_file(
    tx: &Transaction<'_>,
    file: &str,
    hash: &str,
    facts: &ParsedFile,
    line_count: usize,
) -> Result<()> {
    tx.execute(
        "INSERT INTO files VALUES(?,?,?) ON CONFLICT(path) DO UPDATE
                SET hash=excluded.hash,facts=excluded.facts",
        params![file, hash, serde_json::to_string(facts)?],
    )?;
    tx.execute("DELETE FROM symbols WHERE file=?", [file])?;
    let mut stmt = tx.prepare_cached("INSERT OR IGNORE INTO symbols VALUES(?,?,?,?,?)")?;
    for sym in &facts.symbols {
        stmt.execute(params![
            file,
            sym.name,
            sym.kind,
            i64::try_from(sym.start_line)?,
            i64::try_from(sym.end_line)?
        ])?;
    }
    if facts.calls.iter().any(|c| c.caller == MODULE) {
        stmt.execute(params![
            file,
            MODULE,
            "File",
            1,
            i64::try_from(line_count.max(1))?
        ])?;
    }
    Ok(())
}

type Target = (String, String);

struct Module {
    facts: ParsedFile,
    symbol_counts: HashMap<String, usize>,
    imported_files: BTreeSet<String>,
}

impl Module {
    fn new(facts: ParsedFile) -> Self {
        let mut symbol_counts = HashMap::new();
        for symbol in &facts.symbols {
            *symbol_counts.entry(symbol.name.clone()).or_default() += 1;
        }
        Self {
            facts,
            symbol_counts,
            imported_files: BTreeSet::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Binding {
    Local,
    Exported,
}

fn member_name(binding: &str, suffix: &str) -> Option<String> {
    match (binding, suffix) {
        ("*", "") => None,
        ("*", member) => Some(member.to_owned()),
        (name, "") => Some(name.to_owned()),
        (name, member) => Some(format!("{name}.{member}")),
    }
}

struct Resolver {
    php: php::Methods,
    csharp: csharp_index::Bindings,
    ts_paths: crate::tsconfig::TsPaths,
    modules: BTreeMap<String, Module>,
}

fn python_base(from: &str, specifier: &str, root_is_package: bool) -> Option<String> {
    let rest = specifier.trim_start_matches('.');
    let dots = specifier.len() - rest.len();
    let mut parts: Vec<_> = Vec::new();
    if dots > 0 {
        if let Some((package, _)) = from.rsplit_once('/') {
            parts.extend(package.split('/'));
        }
        for _ in 1..dots {
            parts.pop()?;
        }
        if parts.is_empty() && !root_is_package {
            return None;
        }
    } else if rest.is_empty() {
        return None;
    }
    if !rest.is_empty() {
        for part in rest.split('.') {
            if part.is_empty() || !part.chars().all(|c| c == '_' || c.is_alphanumeric()) {
                return None;
            }
            parts.push(part);
        }
    }
    Some(parts.join("/"))
}

fn import_suffix<'a>(name: &'a str, local: &str) -> Option<&'a str> {
    if local.is_empty() {
        return None;
    }
    let rest = name.strip_prefix(local)?;
    if rest.is_empty() {
        Some(rest)
    } else {
        rest.strip_prefix('.')
    }
}

impl Resolver {
    fn python_file(&self, base: &str) -> Option<String> {
        let package = if base.is_empty() {
            "__init__.py".to_owned()
        } else {
            format!("{base}/__init__.py")
        };
        [package, format!("{base}.py")]
            .into_iter()
            .find(|p| self.modules.contains_key(p))
    }

    fn imported_target(&self, file: &str, import: &Import, suffix: &str) -> Option<Target> {
        let member = member_name(&import.imported, suffix)?;
        if !file.ends_with(".py") {
            return Some((self.module_path(file, &import.module)?, member));
        }
        let base = python_base(
            file,
            &import.module,
            self.modules.contains_key("__init__.py"),
        )?;
        let module = self.python_file(&base);

        // "from pkg import x" can bind pkg.x when x is a submodule. An
        // explicit export from __init__.py wins, except when it is this very import.
        let could_be_submodule = import.imported != "*"
            && !suffix.is_empty()
            && module.as_deref().is_none_or(|path| {
                (path == "__init__.py" || path.ends_with("/__init__.py"))
                    && !self.modules[path].facts.exports.iter().any(|e| {
                        e.name == import.imported
                            && !(path == file && e.local == import.local && e.module.is_none())
                    })
            });
        if could_be_submodule {
            let child = if base.is_empty() {
                import.imported.clone()
            } else {
                format!("{base}/{}", import.imported)
            };
            if let Some(child) = self.python_file(&child) {
                return Some((child, suffix.to_owned()));
            }
        }
        Some((module?, member))
    }

    fn module_path(&self, from: &str, specifier: &str) -> Option<String> {
        if !specifier.starts_with("./") && !specifier.starts_with("../") {
            return self
                .ts_paths
                .resolve(from, specifier, |p| self.modules.contains_key(p));
        }
        let mut base = PathBuf::from(from).parent()?.to_owned();
        for component in Path::new(specifier).components() {
            match component {
                Component::CurDir => {}
                Component::Normal(part) => base.push(part),
                Component::ParentDir if base.pop() => {}
                _ => return None,
            }
        }
        let base = base.to_str()?.replace(std::path::MAIN_SEPARATOR, "/");
        let mut candidates = vec![base.clone()];
        let replacements: &[&str] = match Path::new(&base).extension().and_then(|p| p.to_str()) {
            Some("js") => &["ts", "tsx", "jsx"],
            Some("mjs") => &["mts"],
            Some("cjs") => &["cts"],
            Some("jsx") => &["tsx"],
            _ => &[],
        };
        for ext in replacements {
            candidates.push(
                Path::new(&base)
                    .with_extension(ext)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        for ext in JS_EXTENSIONS {
            candidates.push(format!("{base}.{ext}"));
        }
        for ext in JS_EXTENSIONS {
            candidates.push(format!("{base}/index.{ext}"));
        }
        candidates
            .into_iter()
            .find(|p| self.modules.contains_key(p))
    }

    fn resolve_binding(&self, origin: &str, name: &str, line: usize) -> Option<Target> {
        // A barrel can both traverse a cycle and reach the same declaration by
        // different routes. Track lookup states and compare terminal identities.
        let mut pending = vec![(Binding::Local, origin.to_owned(), name.to_owned())];
        let mut seen = HashSet::new();
        let mut resolved = None;
        while let Some(state) = pending.pop() {
            if !seen.insert(state.clone()) {
                continue;
            }
            let (binding, file, name) = state;
            let module = &self.modules[&file];
            let (head, tail) = name.split_once('.').unwrap_or((&name, ""));
            match binding {
                Binding::Local => {
                    if let Some(count) = module.symbol_counts.get(&name) {
                        let target = (file, name);
                        if *count != 1 || resolved.as_ref().is_some_and(|other| other != &target) {
                            return None;
                        }
                        resolved = Some(target);
                        continue;
                    }
                    let local = module
                        .facts
                        .python
                        .scoped_imports
                        .iter()
                        .filter(|i| {
                            file == origin
                                && i.start <= line
                                && line <= i.end
                                && import_suffix(&name, &i.binding.local).is_some()
                        })
                        .min_by_key(|i| i.end - i.start);
                    let imports = if let Some(scoped) = local {
                        std::slice::from_ref(&scoped.binding)
                    } else {
                        &module.facts.imports[..]
                    };
                    let longest = imports
                        .iter()
                        .filter(|i| import_suffix(&name, &i.local).is_some())
                        .map(|i| i.local.len())
                        .max();
                    for import in imports.iter().filter(|i| Some(i.local.len()) == longest) {
                        if let Some(suffix) = import_suffix(&name, &import.local)
                            && let Some((other, target)) =
                                self.imported_target(&file, import, suffix)
                        {
                            pending.push((Binding::Exported, other, target));
                        }
                    }
                }
                Binding::Exported => {
                    let explicit = module.facts.exports.iter().any(|e| e.name == head);
                    for export in &module.facts.exports {
                        let target = if explicit && export.name == head {
                            let Some(target) = member_name(&export.local, tail) else {
                                continue;
                            };
                            target
                        } else if !explicit && export.name == "*" && head != "default" {
                            name.clone()
                        } else {
                            continue;
                        };
                        match &export.module {
                            Some(specifier) => {
                                if let Some(other) = self.module_path(&file, specifier) {
                                    pending.push((Binding::Exported, other, target));
                                }
                            }
                            None => pending.push((Binding::Local, file.clone(), target)),
                        }
                    }
                }
            }
        }
        resolved
    }

    fn python_member(&self, file: &str, member: &crate::model::PythonMember) -> Option<Target> {
        use crate::model::PythonType;
        let class = match &member.receiver {
            PythonType::Builtin => return None,
            PythonType::Named(annotation) | PythonType::Parameter(annotation) => {
                self.resolve_binding(file, annotation_name(annotation, None)?, member.line)?
            }
            PythonType::Returned { callee, element } => {
                let callee = self.resolve(file, &member.caller, callee, member.line)?;
                let module = &self.modules[&callee.0];
                if module
                    .facts
                    .symbols
                    .iter()
                    .any(|s| s.name == callee.1 && s.kind == "Class")
                {
                    if element.is_some() {
                        return None;
                    }
                    callee
                } else {
                    let definition = module.facts.python.definitions.get(&callee.1)?;
                    let annotation = annotation_name(definition.returns.as_deref()?, *element)?;
                    if definition.parent.is_some() {
                        return None;
                    }
                    let result = self.resolve_binding(&callee.0, annotation, 0)?;
                    if result.0 != file && !self.modules[file].imported_files.contains(&result.0) {
                        return None;
                    }
                    result
                }
            }
        };
        let mut pending = vec![class];
        let mut seen = HashSet::new();
        while let Some((path, class)) = pending.pop() {
            if !seen.insert((path.clone(), class.clone())) {
                continue;
            }
            let module = &self.modules[&path];
            let method = format!("{class}.{}", member.attr);
            if let Some(def) = module.facts.python.definitions.get(&method) {
                if !member.property
                    || def
                        .modifiers
                        .as_deref()
                        .is_some_and(|m| m.split(',').any(|v| v == "property"))
                {
                    return Some((path, method));
                }
                return None;
            }
            for base in module
                .facts
                .python
                .definitions
                .get(&class)?
                .bases
                .iter()
                .rev()
            {
                if let Some(parent) = self.resolve_binding(&path, base, 0) {
                    pending.push(parent);
                }
            }
        }
        None
    }

    fn resolve(&self, file: &str, caller: &str, name: &str, line: usize) -> Option<Target> {
        let sql = &self.modules[file].facts.sql;
        if !sql.declarations.is_empty() {
            let method = name.rsplit('.').next()?;
            let mut candidates = sql.declarations.iter().filter(|(_, d)| d.name == method);
            let (identity, _) = candidates.next()?;
            return candidates
                .next()
                .is_none()
                .then(|| (file.into(), identity.clone()));
        }
        if file.ends_with(".rs")
            && let Some((class, method)) = name.rsplit_once("::")
        {
            return self.resolve_binding(file, &format!("{class}.{method}"), line);
        }
        if let Some(site) = self.modules[file].facts.syntax.site(caller, name, line) {
            if file.ends_with(".php") {
                return self.php_call(file, site);
            }
            if file.ends_with(".cs") {
                return self.csharp_call(file, site);
            }
            return site
                .target
                .as_ref()
                .map(|target| (file.into(), target.clone()));
        }
        if let Some(site) = self.modules[file].facts.cfamily.site(caller, name, line) {
            return site.target.as_ref().map(|n| (file.into(), n.clone()));
        }
        if file.ends_with(".go") {
            let module = &self.modules[file];
            if module
                .facts
                .go
                .selector(caller, name, line)
                .is_some_and(|s| !s.own)
            {
                return None;
            }
            return (module.symbol_counts.get(name) == Some(&1))
                .then(|| (file.into(), name.into()));
        }
        if let Some((class, method)) = name.rsplit_once('.')
            && !self.modules[file].facts.typescript.bases.is_empty()
        {
            let mut stack = vec![(file.to_owned(), class.to_owned())];
            let mut seen = HashSet::new();
            while let Some((path, owner)) = stack.pop() {
                if !seen.insert((path.clone(), owner.clone())) {
                    continue;
                }
                let candidate = format!("{owner}.{method}");
                if self.modules[&path].symbol_counts.get(&candidate) == Some(&1) {
                    return Some((path, candidate));
                }
                if let Some(parents) = self.modules[&path].facts.typescript.bases.get(&owner) {
                    stack.extend(
                        parents
                            .iter()
                            .filter_map(|base| self.resolve_binding(&path, base, 0)),
                    );
                }
            }
        }
        if !self.modules[file].facts.bash_imports.is_empty() {
            if let Some(local) = self.resolve_binding(file, name, line) {
                return Some(local);
            }
            let mut found = BTreeSet::new();
            for (_, path) in &self.modules[file].facts.bash_imports {
                let Some(path) = crate::parser::bash::sourced_file(file, path) else {
                    continue;
                };
                if let Some(module) = self.modules.get(&path)
                    && module.symbol_counts.get(name) == Some(&1)
                {
                    found.insert((path, name.to_owned()));
                }
            }
            if found.len() == 1 {
                return found.pop_first();
            }
            return None;
        }
        // A nested definition shadows outer bindings. Sibling methods require a receiver.
        if file.ends_with(".py") {
            let module = &self.modules[file];
            if !name.contains('.')
                && let Some(symbol) = module
                    .facts
                    .symbols
                    .iter()
                    .find(|s| s.name.rsplit('.').next() == Some(name))
            {
                if module.facts.python.ambiguities.contains(&symbol.name) {
                    return (caller.split(":S").next() == Some(symbol.name.as_str()))
                        .then(|| (file.to_owned(), caller.to_owned()));
                }
                return Some((file.to_owned(), symbol.name.clone()));
            }
            if module.facts.python.ambiguities.contains(name) {
                return (caller.split(":S").next() == Some(name))
                    .then(|| (file.to_owned(), caller.to_owned()));
            }
            if name.contains('.') {
                return self.resolve_binding(file, name, line);
            }
            if let Some(target) = self.resolve_binding(file, name, 0) {
                return Some(target);
            }
            let mut candidates = module.imported_files.iter().flat_map(|path| {
                self.modules[path]
                    .facts
                    .symbols
                    .iter()
                    .filter(move |s| s.name.rsplit('.').next() == Some(name))
                    .map(move |s| (path.clone(), s.name.clone()))
            });
            let first = candidates.next()?;
            return candidates.next().is_none().then_some(first);
        }
        let mut scope = caller;
        loop {
            let qualified = format!("{scope}.{name}");
            if let Some(count) = self.modules[file].symbol_counts.get(&qualified) {
                return (*count == 1).then(|| (file.to_owned(), qualified));
            }
            let Some((parent, _)) = scope.rsplit_once('.') else {
                break;
            };
            if self.modules[file]
                .facts
                .symbols
                .iter()
                .any(|s| s.name == parent && s.kind == "Class")
            {
                break;
            }
            scope = parent;
        }
        self.resolve_binding(file, name, line)
    }
}

pub(crate) fn annotation_name(annotation: &str, index: Option<usize>) -> Option<&str> {
    let value = annotation.trim().trim_matches(['\'', '"']).trim();
    if let Some(index) = index {
        let items = value
            .strip_prefix("tuple[")
            .or_else(|| value.strip_prefix("Tuple["))?
            .strip_suffix(']')?;
        let mut depth = 0;
        let mut start = 0;
        let mut position = 0;
        for (i, c) in items
            .char_indices()
            .chain(std::iter::once((items.len(), ',')))
        {
            match c {
                '[' | '(' => depth += 1,
                ']' | ')' => depth -= 1,
                ',' if depth == 0 => {
                    if position == index {
                        return annotation_name(&items[start..i], None);
                    }
                    position += 1;
                    start = i + 1;
                }
                _ => {}
            }
        }
        return None;
    }
    if let Some(inner) = value
        .strip_prefix("Optional[")
        .or_else(|| value.strip_prefix("typing.Optional["))
        .and_then(|v| v.strip_suffix(']'))
    {
        return annotation_name(inner, None);
    }
    if value.contains('|') {
        let mut options = value.split('|').map(str::trim).filter(|s| *s != "None");
        let first = options.next()?;
        return options.next().is_none().then_some(first);
    }
    Some(value)
}

fn resolve_graph(tx: &Transaction<'_>) -> Result<()> {
    let mut modules = BTreeMap::new();
    let mut query = tx.prepare("SELECT path,facts FROM files")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let file: String = row.get(0)?;
        let json: String = row.get(1)?;
        let facts = serde_json::from_str(&json)
            .with_context(|| format!("reading stored facts for {file}"))?;
        modules.insert(file, Module::new(facts));
    }
    let root: String = tx.query_row("SELECT value FROM metadata WHERE key='root'", [], |r| {
        r.get(0)
    })?;
    let ts_paths = crate::tsconfig::TsPaths::load(Path::new(&root), modules.keys());
    let csharp = csharp_index::Bindings::collect(Path::new(&root), &modules);
    let php = php::Methods::collect(Path::new(&root), &modules);
    let mut resolver = Resolver {
        php,
        modules,
        ts_paths,
        csharp,
    };
    let imports: Vec<_> = resolver
        .modules
        .iter()
        .filter(|(file, _)| file.ends_with(".py"))
        .map(|(file, module)| {
            let targets = module
                .facts
                .imports
                .iter()
                .chain(
                    module
                        .facts
                        .python
                        .scoped_imports
                        .iter()
                        .map(|i| &i.binding),
                )
                .filter_map(|i| {
                    resolver.python_file(&python_base(
                        file,
                        &i.module,
                        resolver.modules.contains_key("__init__.py"),
                    )?)
                })
                .collect();
            (file.clone(), targets)
        })
        .collect();
    for (file, targets) in imports {
        resolver.modules.get_mut(&file).unwrap().imported_files = targets;
    }
    tx.execute("DELETE FROM calls", [])?;
    tx.execute("DELETE FROM call_sites", [])?;
    let mut edges = tx.prepare_cached("INSERT OR IGNORE INTO calls VALUES(?,?,?,?,?)")?;
    let mut sites = tx.prepare_cached("INSERT OR IGNORE INTO call_sites VALUES(?,?,?,?,?,?)")?;
    for (file, module) in &resolver.modules {
        let mut add = |caller: &str, raw: &str, target: Target, line: usize| -> Result<()> {
            let line = i64::try_from(line)?;
            edges.execute(params![file, caller, target.0, target.1, line])?;
            sites.execute(params![file, caller, raw, target.0, target.1, line])?;
            Ok(())
        };
        for call in &module.facts.calls {
            if call.caller != MODULE && module.symbol_counts.get(&call.caller) != Some(&1) {
                continue;
            }
            if let Some(target) = resolver.resolve(file, &call.caller, &call.callee, call.line) {
                add(&call.caller, &call.callee, target, call.line)?;
            }
        }
        for member in &module.facts.python.members {
            if let Some(target) = resolver.python_member(file, member) {
                add(&member.caller, &member.raw, target, member.line)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_reexport_wins_over_ambiguous_stars_in_a_cycle() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        fs::write(root.join("a.ts"), "export function run() {}")?;
        fs::write(root.join("b.ts"), "export function run() {}")?;
        fs::write(root.join("loop.ts"), "export * from './barrel';")?;
        let barrel = "export * from './a'; export * from './b'; export * from './loop';";
        fs::write(root.join("barrel.ts"), barrel)?;
        fs::write(
            root.join("consumer.ts"),
            "import * as ns from './barrel'; export function start() { ns.run(); }",
        )?;
        let mut graph = Graph::open(&root.join("graph.db"))?;
        graph.sync(root, true)?;
        assert!(graph.callees("consumer.ts::start")?.is_empty());
        fs::write(
            root.join("barrel.ts"),
            format!("{barrel} export {{run}} from './b';"),
        )?;
        let update = graph.sync(root, false)?;
        assert_eq!(update.files_parsed, 1);
        assert_eq!(
            graph
                .callees("consumer.ts::start")?
                .iter()
                .map(|h| (&*h.file, &*h.name))
                .collect::<Vec<_>>(),
            [("b.ts", "run")]
        );
        fs::remove_file(root.join("b.ts"))?;
        assert_eq!(graph.sync(root, false)?.files_deleted, 1);
        assert!(graph.callees("consumer.ts::start")?.is_empty());
        Ok(())
    }

    #[test]
    fn resolves_python_package_relative_aliases_and_absolute_modules() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        fs::create_dir_all(root.join("pkg/sub"))?;
        fs::write(root.join("pkg/lib.py"), "def run(): pass\n")?;
        fs::write(root.join("utils.py"), "def stray(): pass\n")?;
        fs::write(
            root.join("pkg/invalid.py"),
            "from ..utils import stray\nstray()\n",
        )?;
        fs::write(
            root.join("pkg/__init__.py"),
            "from . import lib\nfrom .lib import run as forwarded\n",
        )?;
        fs::write(
            root.join("pkg/__main__.py"),
            "from .lib import run\nrun()\n",
        )?;
        fs::write(
            root.join("pkg/sub/use.py"),
            concat!(
                "from .. import forwarded as f\n",
                "from .. import lib as util\n",
                "import pkg.lib\n",
                "import pkg.lib as mod\n",
                "from external import run as external\n",
                "def forwarded(): f()\n",
                "def submodule(): util.run()\n",
                "def dotted(): pkg.lib.run()\n",
                "def aliased(): mod.run()\n",
                "def unknown(): external()\n",
            ),
        )?;
        let mut graph = Graph::open(&root.join("index.db"))?;
        graph.sync(root, true)?;
        let callers: BTreeSet<_> = graph
            .callers("pkg/lib.py::run")?
            .into_iter()
            .map(|hit| hit.name)
            .collect();
        assert_eq!(
            callers,
            [MODULE, "forwarded", "submodule", "dotted", "aliased"]
                .map(str::to_owned)
                .into()
        );
        assert!(graph.callees("pkg/sub/use.py::unknown")?.is_empty());
        assert!(graph.callers("utils.py::stray")?.is_empty());
        fs::write(root.join("pkg/lib.py"), "def renamed(): pass\n")?;
        fs::write(
            root.join("pkg/__init__.py"),
            "from .lib import renamed as forwarded\n",
        )?;
        assert_eq!(graph.sync(root, false)?.files_parsed, 2);
        let after = graph.callers("pkg/lib.py::renamed")?;
        assert_eq!(
            after.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            ["forwarded"]
        );
        Ok(())
    }

    #[test]
    fn failed_update_preserves_previous_committed_graph() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        let database = root.join("graph.db");
        let mut graph = Graph::open(&database)?;
        fs::write(root.join("a.py"), "def old(): pass\n")?;
        graph.sync(root, true)?;
        fs::write(root.join("a.py"), "def replacement(): pass\n")?;
        fs::write(root.join("z.py"), [0xff])?;
        assert!(graph.sync(root, false).is_err());
        let reader = Graph::open_existing(&database)?;
        assert_eq!(reader.symbols("old")?.len(), 1);
        assert!(reader.symbols("replacement")?.is_empty());
        assert_eq!(reader.stats()?.files, 1);
        Ok(())
    }

    #[test]
    fn ignores_build_artifacts_and_gitignored_sources_without_git_metadata() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        fs::write(root.join(".gitignore"), "generated.ts\n")?;
        fs::write(root.join("generated.ts"), "export function generated() {}")?;
        fs::create_dir(root.join("target"))?;
        fs::write(root.join("target/built.rs"), "fn generated() {}")?;
        fs::write(root.join("lib.rs"), "fn source() {}")?;
        assert_eq!(source_files(root)?, ["lib.rs"]);
        Ok(())
    }
}

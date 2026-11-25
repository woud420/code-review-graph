use super::store;
use anyhow::{Result, ensure};
use regex::Regex;
use rusqlite::{Connection, params, params_from_iter, types::Value as Sql};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;

mod snippets;

#[derive(Deserialize)]
#[serde(default)]
pub struct Options {
    pub query: String,
    pub kind: Option<String>,
    pub limit: i64,
    pub detail_level: String,
    pub include_source: bool,
    pub expand_neighbors: bool,
    pub max_source_lines: i64,
    pub provider: Option<String>,
    pub model: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            query: String::new(),
            kind: None,
            limit: 20,
            detail_level: "standard".into(),
            include_source: false,
            expand_neighbors: false,
            max_source_lines: 60,
            provider: None,
            model: None,
        }
    }
}

static PHRASE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""([^"]*)""#).unwrap());
static IDENTIFIERS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"\b[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)+\b",
        r"\b[a-z][a-z0-9]*(?:_[a-z0-9]+)+\b",
        r"\b[A-Z][a-z0-9]+(?:[A-Z][a-z0-9]+)+\b",
    ]
    .iter()
    .map(|p| Regex::new(p).unwrap())
    .collect()
});
static EXACT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_][\w.]*$").unwrap());
static TEST_FILENAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r"^test_.*\.py$|_test\.py$|^conftest\.py$|\.(?:test|spec|stories)\.[cm]?[jt]sx?$|_test\.go$|_(?:test|spec)\.rb$|[a-z0-9]Tests?\.(?:java|kt|kts|scala|groovy|cs)$|[a-z0-9]Test\.php$|_test\.dart$|^test[_-].*\.[rR]$|(?i:\.tests\.ps1)$|[_.]test\.resi?$|^runtests\.jl$"
).unwrap()
});

fn words(text: &str) -> HashSet<String> {
    let mut words = HashSet::new();
    for run in text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
    {
        words.insert(run.to_ascii_lowercase());
        let bytes = run.as_bytes();
        let mut start = 0;
        for i in 1..bytes.len() {
            if bytes[i].is_ascii_uppercase()
                && (!bytes[i - 1].is_ascii_uppercase()
                    || bytes.get(i + 1).is_some_and(u8::is_ascii_lowercase))
            {
                words.insert(run[start..i].to_ascii_lowercase());
                start = i;
            }
        }
        words.insert(run[start..].to_ascii_lowercase());
    }
    words
}

fn expressions(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut loose = Vec::new();
    let mut cursor = 0;
    for matched in PHRASE.captures_iter(query) {
        let whole = matched.get(0).unwrap();
        loose.extend(query[cursor..whole.start()].split_whitespace());
        terms.push((true, matched.get(1).unwrap().as_str()));
        cursor = whole.end();
    }
    loose.extend(query[cursor..].split_whitespace());
    terms.extend(loose.into_iter().map(|term| (false, term)));
    let mut seen = HashSet::new();
    terms.retain(|(_, term)| {
        term.chars().any(|c| c.is_ascii_alphanumeric()) && seen.insert(term.to_lowercase())
    });
    if terms.len() > 12 {
        let mut indices = (0..terms.len()).collect::<Vec<_>>();
        indices.sort_by_key(|&i| {
            (
                !terms[i].0,
                if terms[i].0 {
                    0
                } else {
                    usize::MAX - terms[i].1.chars().count()
                },
                i,
            )
        });
        indices.truncate(12);
        indices.sort_unstable();
        terms = indices.into_iter().map(|i| terms[i]).collect();
    }
    let tokens = terms
        .iter()
        .map(|(phrase, text)| {
            let prefix = !phrase && text.chars().filter(|c| c.is_alphanumeric()).count() >= 3;
            format!(
                "\"{}\"{}",
                text.replace('"', "\"\""),
                if prefix { "*" } else { "" }
            )
        })
        .collect::<Vec<_>>();
    match tokens.len() {
        0 => vec![],
        1 => tokens,
        _ if terms.iter().any(|t| t.0) => vec![tokens.join(" AND ")],
        _ => vec![tokens.join(" AND "), tokens.join(" OR ")],
    }
}

type Ranked = Vec<(i64, f64)>;

pub(super) fn fts(
    conn: &Connection,
    query: &str,
    kind: Option<&str>,
    limit: i64,
    skip_json: bool,
) -> Result<Ranked> {
    let mut columns = conn.prepare("PRAGMA table_info(nodes_fts)")?;
    let weights = columns
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .map(|col| match col.as_str() {
            "name" => "10.0",
            "qualified_name" => "6.0",
            "signature" => "2.0",
            "docstring" => "1.5",
            "name_tokens" => "4.0",
            _ => "1.0",
        })
        .collect::<Vec<_>>()
        .join(", ");
    if weights.is_empty() {
        return Ok(vec![]);
    }
    let sql = format!(
        "SELECT nodes_fts.rowid, -bm25(nodes_fts, {weights}) AS score
        FROM nodes_fts CROSS JOIN nodes n ON n.id=nodes_fts.rowid
        WHERE nodes_fts MATCH ?1 AND ((?2 IS NULL AND n.kind!='Output') OR n.kind=?2)
        AND (NOT ?3 OR n.language IS NOT 'json') ORDER BY score DESC LIMIT ?4"
    );
    let mut stmt = conn.prepare(&sql)?;
    for expr in expressions(query) {
        let rows = stmt
            .query_map(params![expr, kind, skip_json, limit], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        if !Vec::is_empty(&rows) {
            return Ok(rows);
        }
    }
    Ok(vec![])
}

fn keyword(conn: &Connection, query: &str, kind: Option<&str>, limit: i64) -> Result<Ranked> {
    let lower = query.to_lowercase();
    let tokens = lower.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() {
        return Ok(vec![]);
    }
    let mut bindings = Vec::new();
    let mut filters = Vec::new();
    for token in tokens {
        filters.push("(lower(name) LIKE ? OR lower(qualified_name) LIKE ?)");
        let pattern = Sql::Text(format!("%{token}%"));
        bindings.extend([pattern.clone(), pattern]);
    }
    if let Some(k) = kind {
        filters.push("kind=?");
        bindings.push(Sql::Text(k.into()));
    } else {
        filters.push("kind!='Output'");
    }
    bindings.push(Sql::Integer(limit));
    let sql = format!(
        "SELECT id,name FROM nodes WHERE {} LIMIT ?",
        filters.join(" AND ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut hits = stmt
        .query_map(params_from_iter(bindings), |r| {
            let name = r.get::<_, String>(1)?.to_lowercase();
            Ok((
                r.get(0)?,
                if name == lower {
                    3.0
                } else if name.starts_with(&lower) {
                    2.0
                } else {
                    1.0
                },
            ))
        })?
        .collect::<rusqlite::Result<Ranked>>()?;
    hits.sort_by(|a, b| b.1.total_cmp(&a.1));
    Ok(hits)
}

fn test_file(root: &Path, path: &Path) -> bool {
    if TEST_FILENAME.is_match(
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
    ) {
        return true;
    }
    let relative = path.strip_prefix(root).unwrap_or(path);
    if relative.is_absolute() {
        return false;
    }
    relative.parent().is_some_and(|p| {
        p.iter().any(|part| {
            matches!(
                part.to_str(),
                Some(
                    "tests"
                        | "__tests__"
                        | "e2e_test"
                        | "e2e_tests"
                        | "e2e-test"
                        | "e2e-tests"
                        | "testthat"
                )
            ) || (p.iter().any(|part| part == "spec" || part == "specs")
                && matches!(
                    path.extension().and_then(|v| v.to_str()),
                    Some("rb" | "rake")
                ))
                || p.iter().any(|part| part == "test") && !root.join(p).join("__init__.py").exists()
        })
    })
}

struct Boost<'a> {
    query: &'a str,
    lower: String,
    words: HashSet<String>,
    identifiers: Vec<String>,
    test_intent: bool,
    json_intent: bool,
}

impl<'a> Boost<'a> {
    fn new(query: &'a str, kind: Option<&str>) -> Self {
        let words = words(query);
        let test_intent = kind == Some("Test")
            || [
                "test", "tests", "tested", "testing", "spec", "specs", "fixture", "fixtures",
                "mock", "mocks", "stub", "stubs", "coverage",
            ]
            .iter()
            .any(|word| words.contains(*word));
        let identifiers = IDENTIFIERS
            .iter()
            .flat_map(|p| p.find_iter(query))
            .map(|m| m.as_str().to_lowercase())
            .filter(|s| s.chars().count() >= 3)
            .collect();
        let lower = query.trim().to_lowercase();
        let json_intent = kind == Some("JsonPath")
            || query.trim_start().starts_with('$')
            || lower.contains(".json");
        Self {
            query,
            lower,
            words,
            identifiers,
            test_intent,
            json_intent,
        }
    }
    fn score(&self, node: &Value, symbol: &str, is_test: bool, root: &Path, mut score: f64) -> f64 {
        let name = node["name"].as_str().unwrap_or_default();
        let qualified = node["qualified_name"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase();
        let kind = node["kind"].as_str().unwrap_or_default();
        let q = self.query.trim();
        if q.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
            && q.as_bytes().get(1).is_some_and(u8::is_ascii_lowercase)
            && matches!(kind, "Class" | "Type")
        {
            score *= 1.5;
        }
        if q.contains('_') && q.bytes().any(|c| c.is_ascii_alphabetic()) && kind == "Function" {
            score *= 1.5;
        }
        if q.contains('.') && qualified.contains(&self.query.to_lowercase()) {
            score *= 2.;
        }
        if self.identifiers.iter().any(|s| qualified.contains(s)) {
            score *= 2.;
        }
        if !self.words.is_empty() {
            let parts = words(if symbol.is_empty() { name } else { symbol });
            let covered = self
                .words
                .iter()
                .filter(|w| {
                    parts.contains(*w) || w.len() >= 3 && parts.iter().any(|p| p.starts_with(*w))
                })
                .count();
            if covered > 0 {
                score *= 1.0 + covered as f64 / self.words.len() as f64;
                if parts == self.words {
                    score *= 2.;
                }
            }
        }
        if EXACT.is_match(q) {
            if name.to_lowercase() == self.lower {
                score *= 4.;
            } else if qualified.ends_with(&format!("::{}", self.lower)) {
                score *= 3.;
            }
        }
        if node["language"] == "json" && !self.json_intent {
            score *= 0.25;
        }
        if !self.test_intent
            && (is_test
                || test_file(
                    root,
                    Path::new(node["file_path"].as_str().unwrap_or_default()),
                ))
        {
            score *= 0.4;
        }
        score
    }
}

pub(super) fn ranked(
    conn: &Connection,
    root: &Path,
    options: &Options,
    window: i64,
) -> Result<(&'static str, Vec<Value>)> {
    let (mode, rows) = ranked_with_ids(conn, root, options, window)?;
    Ok((mode, rows.into_iter().map(|(_, node)| node).collect()))
}
pub(super) fn ranked_with_ids(
    conn: &Connection,
    root: &Path,
    options: &Options,
    window: i64,
) -> Result<(&'static str, Vec<(i64, Value)>)> {
    let query = &options.query;
    if query.trim().is_empty() {
        return Ok(("none", vec![]));
    }
    let fetch = window.saturating_mul(3);
    let kind = options.kind.as_deref().filter(|s| !s.is_empty());
    let boost = Boost::new(query, kind);
    let mut hits = fts(conn, query, kind, fetch, false)?;
    if !boost.json_intent && hits.len() == fetch as usize {
        let mut json_test = conn.prepare("SELECT language IS 'json' FROM nodes WHERE id=?")?;
        let mut has_json = false;
        for (id, _) in &hits {
            if json_test.query_row([id], |r| r.get::<_, bool>(0))? {
                has_json = true;
                break;
            }
        }
        if has_json {
            let ids: HashSet<_> = hits.iter().map(|(id, _)| *id).collect();
            hits.extend(
                fts(conn, query, kind, fetch, true)?
                    .into_iter()
                    .filter(|(id, _)| !ids.contains(id)),
            );
            hits.sort_by(|a, b| b.1.total_cmp(&a.1));
        }
    }
    let vectors = code_review_graph_rs::canonical::embeddings::rank(
        conn,
        query,
        options.provider.as_deref(),
        options.model.as_deref(),
        kind,
        fetch as usize,
    )
    .unwrap_or_else(|err| {
        eprintln!("WARNING: Embedding search failed: {err}");
        vec![]
    });
    let mode = if hits.is_empty() && vectors.is_empty() {
        hits = keyword(conn, query, kind, fetch)?;
        if hits.is_empty() {
            return Ok(("none", vec![]));
        }
        "keyword"
    } else {
        let mode = match (hits.is_empty(), vectors.is_empty()) {
            (false, false) => "hybrid",
            (true, false) => "semantic",
            _ => "fts",
        };
        let mut merged = indexmap::IndexMap::<i64, f64>::new();
        for ranked in [hits, vectors] {
            for (rank, (id, _)) in ranked.into_iter().enumerate() {
                *merged.entry(id).or_default() += 1.0 / (61 + rank) as f64;
            }
        }
        hits = merged.into_iter().collect();
        hits.sort_by(|a, b| b.1.total_cmp(&a.1));
        mode
    };
    let mut get=conn.prepare("SELECT name,qualified_name,kind,file_path,line_start,line_end,COALESCE(language,''),params,return_type,signature,COALESCE(symbol,''),is_test FROM nodes WHERE id=?")?;
    let mut results = Vec::new();
    for (id, score) in hits {
        let (mut node,symbol,is_test)=get.query_row([id],|r|Ok((json!({
            "name":r.get::<_,String>(0)?, "qualified_name":r.get::<_,String>(1)?,
            "kind":r.get::<_,String>(2)?, "file_path":r.get::<_,String>(3)?,
            "line_start":r.get::<_,i64>(4)?, "line_end":r.get::<_,i64>(5)?,
            "language":r.get::<_,String>(6)?, "params":r.get::<_,Option<String>>(7)?,
            "return_type":r.get::<_,Option<String>>(8)?, "signature":r.get::<_,Option<String>>(9)?
        }),r.get::<_,String>(10)?,r.get::<_,bool>(11)?)))?;
        let score = boost.score(&node, &symbol, is_test, root, score);
        for key in ["name", "qualified_name"] {
            node[key] = json!(clean(node[key].as_str().unwrap_or_default(), 256));
        }
        node["score"] = json!((score * 1e6).round_ties_even() / 1e6);
        results.push((score, (id, node)));
    }
    results.sort_by(|a, b| b.0.total_cmp(&a.0));
    Ok((
        mode,
        results
            .into_iter()
            .take(window as usize)
            .map(|(_, node)| node)
            .collect(),
    ))
}

fn clean(s: &str, limit: usize) -> String {
    s.chars()
        .filter(|c| *c >= ' ' || *c == '\t' || *c == '\n')
        .take(limit)
        .collect()
}

fn confidence(conn: &Connection, root: &Path, query: &str) -> Result<String> {
    if !conn.query_row("SELECT EXISTS(SELECT 1 FROM nodes)", [], |r| {
        r.get::<_, bool>(0)
    })? {
        return Ok("graph is empty: nothing is indexed, so this 0 says nothing about the code; run `code-review-graph build`".into());
    }
    if let Some(sha) = store::metadata(conn, "git_head_sha")? {
        let head = store::git(root, &["rev-parse", "HEAD"]);
        if !sha.is_empty() && !head.is_empty() && sha != head {
            return Ok("graph is stale: built at an older commit than HEAD, so this 0 may be out of date; run `code-review-graph update`".into());
        }
    }
    let prefix = "no indexed node matches '";
    let suffix = "'; search covers names, paths and signatures, not source text";
    let max = 140 - prefix.len() - suffix.len();
    let clean = clean(query, 140)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let fragment = if clean.chars().count() > max {
        format!("{}~", clean.chars().take(max - 1).collect::<String>())
    } else {
        clean
    };
    Ok(format!("{prefix}{fragment}{suffix}"))
}

pub fn run(conn: &Connection, root: &Path, options: &Options) -> Result<Value> {
    ensure!(
        options.limit > 0,
        "limit must be an integer greater than or equal to 1"
    );
    let minimal = options.detail_level == "minimal";
    let shown = options.limit.min(if minimal { 5 } else { 100 });
    let window = shown * 3;
    let (mode, mut hits) = ranked(conn, root, options, window)?;
    let total = hits.len();
    hits.truncate(shown as usize);
    let omitted = if options.include_source && !minimal {
        snippets::attach(conn, root, &mut hits, options)?
    } else {
        0
    };
    if minimal {
        for node in &mut hits {
            *node = json!({"name":node["name"],"kind":node["kind"],"file_path":node["file_path"],"score":node["score"]});
        }
    }
    let lower_bound = total >= window as usize;
    let mut summary = format!(
        "Found {total}{} node(s) matching '{}'",
        if lower_bound { "+" } else { "" },
        options.query
    );
    if let Some(kind) = options.kind.as_ref().filter(|s| !s.is_empty()) {
        summary.push_str(&format!(" (kind={kind})"));
    }
    let omitted_hits = total - hits.len();
    if omitted_hits > 0 {
        summary.push_str(&format!(", showing {} of {total}", hits.len()));
    }
    let mut answer = json!({"status":"ok","query":options.query,"search_mode":mode,"summary":summary,"results":hits,
        "result_count":total,"total":total,"truncated":omitted_hits>0,"results_omitted":omitted_hits});
    if lower_bound {
        answer["total_is_lower_bound"] = json!(true);
    }
    if omitted > 0 {
        answer["source_omitted"] = json!(omitted);
    }
    if total == 0 {
        answer["confidence"] = json!(confidence(conn, root, &options.query)?);
    }
    if !minimal {
        answer["_hints"] = json!({"next_steps":[
        {"tool":"query_graph_tool","suggestion":"Inspect callers/callees of a search result"},
        {"tool":"get_flow_tool","suggestion":"See the execution flow through a matched node"},
        {"tool":"get_impact_radius_tool","suggestion":"Check the blast radius from matched nodes"}],
        "related":[],"warnings":[]});
    }
    Ok(answer)
}

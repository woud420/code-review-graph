mod provider;
use anyhow::Result;
use provider::Provider;
use rusqlite::{Connection, Row, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, time::Duration};

pub fn write(
    path: &Path,
    provider: Option<&str>,
    model: Option<&str>,
    wait: bool,
    check: impl FnOnce(&Connection) -> Result<()>,
) -> Result<Value> {
    super::ensure_writable(path)?;
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing graph directory"))?;
    fs::create_dir_all(dir)?;
    let _lock = super::WriteLock::acquire(dir, wait)?;
    let mut conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(30))?;
    if conn.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        let tx = conn.transaction()?;
        tx.execute_batch(include_str!("../schema.sql"))?;
        super::set_metadata(&tx, "schema_version", "15")?;
        tx.commit()?;
        eprintln!("INFO: Created graph database (schema v15).");
    }
    check(&conn)?;
    let selected = match Provider::select(provider, model) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ERROR: embed_graph: {e}");
            return Ok(json!({"status":"error","error":e.to_string()}));
        }
    };
    schema(&conn)?;
    let Some(backend) = selected else {
        let err = if provider
            .is_some_and(|p| matches!(p, "openai" | "google" | "minimax" | "voyage"))
        {
            format!(
                "The '{}' embedding provider is not available. Check the required environment variables (see README and `get_provider()` docstring) and that the endpoint is reachable.",
                provider.unwrap_or_default()
            )
        } else {
            String::from(
                "The local embedding provider needs sentence-transformers. Install with: pip install \"code-review-graph[embeddings]\" — or switch provider to 'openai' / 'google' / 'minimax' / 'voyage'.",
            )
        };
        return Ok(json!({"status":"error","error":err}));
    };
    let (added, total, _) = index(&mut conn, &backend)?;
    let status = if total > 0 {
        "Semantic search is now active."
    } else {
        "No embeddings are available for semantic search."
    };
    Ok(
        json!({"status":"ok","summary":format!("Embedded {added} new node(s). Total embeddings: {total}. {status}"),"newly_embedded":added,"total_embeddings":total}),
    )
}
fn schema(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS embeddings (qualified_name TEXT PRIMARY KEY, vector BLOB NOT NULL, text_hash TEXT NOT NULL, provider TEXT NOT NULL DEFAULT 'unknown')")?;
    if !conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('embeddings') WHERE name='provider')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        conn.execute(
            "ALTER TABLE embeddings ADD COLUMN provider TEXT NOT NULL DEFAULT 'unknown'",
            [],
        )?;
    }
    Ok(())
}
fn split_name(name: &str) -> String {
    let mut previous = ' ';
    let mut out = String::new();
    for ch in name.chars() {
        if previous.is_ascii_lowercase() && ch.is_ascii_uppercase() {
            out.push(' ');
        }
        out.push(if matches!(ch, '_' | '.' | '/' | '-') {
            ' '
        } else {
            ch
        });
        previous = ch;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn node_text(row: &Row<'_>) -> rusqlite::Result<String> {
    let name = row.get::<_, String>("name")?;
    let parent = row
        .get::<_, Option<String>>("parent_name")?
        .unwrap_or_default();
    let mut parts = Vec::new();
    if !parent.is_empty() {
        parts.push(format!("{parent}.{name}"));
    }
    parts.push(name.clone());
    let split = split_name(&name);
    if !split.is_empty() && split.to_lowercase() != name.to_lowercase() {
        parts.push(split);
    }
    parts.push(row.get::<_, String>("kind")?.to_lowercase());
    if !parent.is_empty() {
        parts.push(format!("in {parent}"));
        let split = split_name(&parent);
        if !split.is_empty() && split.to_lowercase() != parent.to_lowercase() {
            parts.push(split);
        }
    }
    if let Some(params) = row
        .get::<_, Option<String>>("params")?
        .filter(|s| !s.is_empty())
    {
        parts.push(params);
    }
    if let Some(ret) = row
        .get::<_, Option<String>>("return_type")?
        .filter(|s| !s.is_empty())
    {
        parts.push(format!("returns {ret}"));
    }
    let extra: Value = row
        .get::<_, Option<String>>("extra")?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if let Some(doc) = extra["docstring"].as_str() {
        let text = doc.split_whitespace().collect::<Vec<_>>().join(" ");
        if !text.is_empty() {
            parts.push(text.chars().take(400).collect());
        }
    }
    let file: String = row.get("file_path")?;
    if let Some(dir) = Path::new(&file)
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .filter(|n| !matches!(*n, "." | "src" | "lib"))
    {
        parts.push(dir.to_owned());
    }
    if let Some(lang) = row
        .get::<_, Option<String>>("language")?
        .filter(|s| !s.is_empty())
    {
        parts.push(lang);
    }
    Ok(parts.join(" "))
}
fn index(conn: &mut Connection, backend: &Provider) -> Result<(usize, i64, usize)> {
    let purged=conn.execute("DELETE FROM embeddings WHERE NOT EXISTS(SELECT 1 FROM nodes WHERE nodes.qualified_name=embeddings.qualified_name)",[])?;
    let todo = {
        let mut stmt=conn.prepare("SELECT n.*,e.text_hash AS previous FROM nodes n LEFT JOIN embeddings e ON e.qualified_name=n.qualified_name AND e.provider=?1 WHERE n.kind!='File' ORDER BY n.id")?;
        let mut query = stmt.query([&backend.identity])?;
        let mut todo = Vec::new();
        while let Some(r) = query.next()? {
            let text = node_text(r)?;
            let hash = hex::encode(Sha256::digest(text.as_bytes()));
            if r.get::<_, Option<String>>("previous")?.as_deref() != Some(&hash) {
                todo.push((r.get::<_, String>("qualified_name")?, text, hash));
            }
        }
        todo
    };
    for batch in todo.chunks(64) {
        let texts: Vec<_> = batch.iter().map(|item| item.1.clone()).collect();
        let vectors = backend.vectors(&texts, false)?;
        let tx = conn.transaction()?;
        for ((name, _, hash), vec) in batch.iter().zip(vectors) {
            let bytes: Vec<u8> = vec
                .into_iter()
                .flat_map(|f| (f as f32).to_ne_bytes())
                .collect();
            tx.execute("INSERT OR REPLACE INTO embeddings(qualified_name,vector,text_hash,provider) VALUES(?,?,?,?)",params![name,bytes,hash,backend.identity])?;
        }
        tx.commit()?;
    }
    Ok((
        todo.len(),
        conn.query_row("SELECT count(*) FROM embeddings", [], |r| r.get(0))?,
        purged,
    ))
}
pub fn rank(
    conn: &Connection,
    query: &str,
    provider: Option<&str>,
    model: Option<&str>,
    kind: Option<&str>,
    limit: usize,
) -> Result<Vec<(i64, f64)>> {
    if std::env::var("CRG_READ_ONLY").is_ok_and(|s| {
        matches!(
            s.trim().to_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        )
    }) {
        return Ok(vec![]);
    }
    if !conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='embeddings')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(vec![]);
    }
    if !conn.query_row("SELECT EXISTS(SELECT 1 FROM embeddings)", [], |r| {
        r.get::<_, bool>(0)
    })? {
        return Ok(vec![]);
    }
    let Some(backend) = Provider::select(provider, model)? else {
        return Ok(vec![]);
    };
    let vector = backend
        .vectors(&[query.to_owned()], true)?
        .pop()
        .unwrap_or_default();
    let norm = vector.iter().map(|v| v * v).sum::<f64>().sqrt();
    let mut stmt=conn.prepare("SELECT n.id,e.vector FROM embeddings e JOIN nodes n ON n.qualified_name=e.qualified_name WHERE e.provider=?1 AND (?2 IS NULL OR n.kind=?2) ORDER BY e.rowid")?;
    let mut rows = stmt.query(params![backend.identity, kind])?;
    let mut ranked = Vec::new();
    while let Some(r) = rows.next()? {
        let data: Vec<u8> = r.get(1)?;
        let mut dot = 0.;
        let mut ss = 0.;
        if data.len() == vector.len() * 4 {
            for (chunk, v) in data.as_chunks::<4>().0.iter().zip(&vector) {
                let f = f32::from_ne_bytes(*chunk) as f64;
                dot += v * f;
                ss += f * f;
            }
        }
        ranked.push((
            r.get(0)?,
            if norm > 0. && ss > 0. {
                dot / (norm * ss.sqrt())
            } else {
                0.
            },
        ));
    }
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    ranked.truncate(limit);
    Ok(ranked)
}

pub fn available() -> Result<bool> {
    Ok(Provider::select(None, None)?.is_some())
}

#[derive(Default)]
pub enum Refresh {
    #[default]
    NotRequested,
    Empty,
    Updated {
        embedded: usize,
        purged: usize,
    },
    Failed {
        class: &'static str,
        reason: String,
    },
}
pub(super) fn refresh(conn: &mut Connection, request: Option<(&str, &str)>) -> Refresh {
    let Some((name, model)) = request else {
        return Refresh::NotRequested;
    };
    let normalized = name.trim().to_lowercase();
    let model = model.trim();
    let value = |reason: &str| Refresh::Failed {
        class: "ValueError",
        reason: reason.into(),
    };
    if normalized.is_empty() || model.is_empty() {
        return value("Embedding refresh requires an explicit provider and model.");
    }
    match refresh_existing(conn, &normalized, model) {
        Ok(v) => v,
        Err(e) => Refresh::Failed {
            class: "RuntimeError",
            reason: e.to_string(),
        },
    }
}
fn refresh_existing(conn: &mut Connection, name: &str, model: &str) -> Result<Refresh> {
    if !has_table(conn)?
        || !conn.query_row("SELECT EXISTS(SELECT 1 FROM embeddings)", [], |r| {
            r.get::<_, bool>(0)
        })?
    {
        return Ok(Refresh::Empty);
    }
    let value = |reason: String| Refresh::Failed {
        class: "ValueError",
        reason,
    };
    if !conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('embeddings') WHERE name='provider')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(value("Embedding refresh refused: existing rows have no provider identity; run an explicit embed to migrate and rebuild the index.".into()));
    }
    let identities: Vec<String> = conn
        .prepare("SELECT DISTINCT provider FROM embeddings ORDER BY provider")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let provider = match Provider::select(Some(name), Some(model)) {
        Ok(Some(p)) => p,
        Ok(None) => {
            anyhow::bail!("Embedding provider '{name}' is unavailable in this environment.")
        }
        Err(e) => return Ok(value(e.to_string())),
    };
    if name == "minimax" && model != "embo-01" {
        return Ok(value(format!(
            "MiniMax refresh model must be 'embo-01', got '{model}'."
        )));
    }
    if identities.len() != 1 || identities[0] != provider.identity {
        return Ok(value(format!(
            "Embedding refresh refused: existing embeddings use {}; requested provider resolves to {}.",
            identities.join(", "),
            provider.identity
        )));
    }
    let (embedded, _, purged) = index(conn, &provider)?;
    Ok(Refresh::Updated { embedded, purged })
}
pub(super) fn has_table(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='embeddings')",
        [],
        |r| r.get::<_, bool>(0),
    )?)
}

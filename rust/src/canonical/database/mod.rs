#[cfg(test)]
mod cache_upgrade;
pub mod migrate;
use anyhow::{Result, bail, ensure};
use rusqlite::{Connection, Error, OptionalExtension};
use std::path::Path;

pub fn permissions(db: &Path) -> Result<()> {
    let Some(dir) = db.parent() else {
        return Ok(());
    };
    if dir.metadata().is_ok_and(|m| m.permissions().readonly()) && !super::read_only() {
        bail!(
            "Error: cannot open the graph database for writing at {} (attempt to write a readonly database). Check the permissions on {}, or set CRG_DATA_DIR to a writable directory.",
            db.display(),
            dir.display()
        );
    }
    Ok(())
}

fn unreadable(db: &Path, error: Error) -> anyhow::Error {
    let message = match error {
        Error::SqliteFailure(_, Some(detail)) => detail,
        e => e.to_string(),
    };
    anyhow::anyhow!(
        "Error: the graph database at {} is unreadable ({message}). Run `code-review-graph build` to rebuild it from scratch.",
        db.display()
    )
}

pub fn validate(conn: &Connection, db: &Path) -> Result<()> {
    let tables = (|| -> rusqlite::Result<Vec<String>> {
        conn.prepare("SELECT name FROM sqlite_master WHERE type IN ('table','view') AND name NOT GLOB 'sqlite_*' ORDER BY name")?
            .query_map([], |r| r.get(0))?.collect()
    })().map_err(|e| unreadable(db, e))?;
    if !tables.is_empty() && !tables.iter().any(|t| t == "nodes" || t == "metadata") {
        let sample = tables
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "Error: {} is a SQLite database but not a code-review-graph graph (it holds {sample}). Point --data-dir somewhere else, or delete the file and run `code-review-graph build`.",
            db.display()
        );
    }
    if tables.iter().any(|t| t == "metadata") {
        let version: Option<String> = conn
            .query_row(
                "SELECT value FROM metadata WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| unreadable(db, e))?;
        if let Some(version) = version
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| *v > 15)
        {
            let exe = std::env::current_exe()?;
            bail!(
                "Error: the graph database at {} was written by a newer code-review-graph (schema v{version}; this build understands v15). This is code-review-graph 2.4.0 at {}. Upgrade this copy, or delete the file and run `code-review-graph build`. Your MCP entry and hooks may run a different copy: `code-review-graph doctor` lists each one and its version.",
                db.display(),
                exe.display()
            );
        }
    }
    Ok(())
}

pub(super) fn verify_owner(conn: &Connection, root: &Path) -> Result<()> {
    if let Some(owner) = super::metadata(conn, "repo_root")? {
        ensure!(
            Path::new(&owner) == root,
            "index belongs to another repository"
        );
    } else {
        let mut files = conn.prepare("SELECT file_path FROM nodes WHERE kind='File'")?;
        for row in files.query_map([], |row| row.get::<_, String>(0))? {
            ensure!(
                Path::new(&row?).starts_with(root),
                "index belongs to another repository"
            );
        }
    }
    Ok(())
}

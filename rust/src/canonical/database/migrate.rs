use super::super::{index_tokens, metadata, set_metadata};
use anyhow::{Result, ensure};
use rusqlite::{Connection, Transaction};
use std::path::Path;

fn version(conn: &Connection) -> Result<Option<u32>> {
    if !conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='metadata' AND type='table')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(None);
    }
    Ok(metadata(conn, "schema_version")?.and_then(|s| s.parse().ok()))
}

pub fn before_read(path: &Path, conn: &Connection) -> Result<()> {
    let Some(old @ 1..=14) = version(conn)? else {
        return Ok(());
    };
    ensure!(
        !super::super::read_only(),
        "The graph at {} is schema v{old}, this build needs v15, and a read-only open cannot upgrade it. Run `code-review-graph update` first.",
        path.display()
    );
    let mut writer =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let tx = writer.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    upgrade(&tx, false)?;
    tx.commit()?;
    Ok(())
}

fn add(tx: &Transaction<'_>, table: &str, column: &str, declaration: &str) -> Result<bool> {
    let exists = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
        [table, column],
        |r| r.get::<_, bool>(0),
    )?;
    if !exists {
        tx.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
        ))?;
    }
    Ok(!exists)
}

pub(crate) fn upgrade(tx: &Transaction<'_>, quiet: bool) -> Result<()> {
    let Some(old @ 1..=14) = version(tx)? else {
        return Ok(());
    };
    let tables = tx.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('nodes','edges')",
        [],
        |r| r.get::<_, i64>(0),
    )?;
    ensure!(tables == 2, "legacy graph is missing nodes or edges");
    let mut steps = Vec::new();
    for (v, table, column, decl) in [
        (2, "nodes", "signature", "TEXT"),
        (4, "nodes", "community_id", "INTEGER"),
        (9, "edges", "confidence", "REAL DEFAULT 1.0"),
        (9, "edges", "confidence_tier", "TEXT DEFAULT 'EXTRACTED'"),
        (10, "nodes", "symbol", "TEXT"),
        (11, "edges", "target_resolution", "TEXT"),
        (13, "nodes", "docstring", "TEXT"),
        (13, "nodes", "name_tokens", "TEXT"),
    ] {
        if old < v && add(tx, table, column, decl)? {
            steps.push(column);
        }
    }
    // The FTS table uses the old external-content column list through v12.
    if old < 13 {
        tx.execute_batch("DROP TABLE IF EXISTS nodes_fts; DROP TABLE IF EXISTS nodes_fts_state;")?;
    }
    let ddl = include_str!("../schema.sql")
        .replace("CREATE TABLE ", "CREATE TABLE IF NOT EXISTS ")
        .replace(
            "CREATE VIRTUAL TABLE ",
            "CREATE VIRTUAL TABLE IF NOT EXISTS ",
        )
        .replace("CREATE INDEX ", "CREATE INDEX IF NOT EXISTS ");
    tx.execute_batch(&ddl)?;
    if old < 10 {
        tx.execute_batch("UPDATE nodes SET symbol=CASE WHEN instr(qualified_name,'::')>0 THEN substr(qualified_name,instr(qualified_name,'::')+2) ELSE qualified_name END;")?;
    }
    if old < 11 {
        tx.execute_batch("UPDATE edges SET target_resolution=CASE WHEN EXISTS (SELECT 1 FROM nodes n WHERE n.qualified_name=edges.target_qualified) THEN 'direct' ELSE 'unresolved' END WHERE kind IN ('CALLS','REFERENCES');")?;
    }
    if old < 13 {
        tx.execute_batch("UPDATE nodes SET docstring=substr(trim(json_extract(extra,'$.docstring')),1,400) WHERE docstring IS NULL AND extra IS NOT NULL AND json_valid(extra) AND json_type(extra,'$.docstring')='text';")?;
        let mut read =
            tx.prepare("SELECT id,kind,name,parent_name,file_path FROM nodes ORDER BY id")?;
        let mut write = tx.prepare("UPDATE nodes SET name_tokens=?1 WHERE id=?2")?;
        let mut rows = read.query([])?;
        while let Some(row) = rows.next()? {
            let parent: Option<String> = row.get(3)?;
            let tokens = index_tokens(
                &row.get::<_, String>(2)?,
                &row.get::<_, String>(1)?,
                &row.get::<_, String>(4)?,
                parent.as_deref(),
            );
            write.execute(rusqlite::params![tokens, row.get::<_, i64>(0)?])?;
        }
        tx.execute_batch("INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild'); INSERT INTO nodes_fts_state(node_id,name,qualified_name,file_path,signature,docstring,name_tokens) SELECT id,name,qualified_name,file_path,signature,docstring,name_tokens FROM nodes;")?;
        set_metadata(tx, "fts_state_synced", "2")?;
    }
    let mut ruby = 0;
    if old < 14 {
        ruby=tx.execute("UPDATE nodes SET file_hash='' WHERE kind='File' AND language='ruby' AND file_hash IS NOT NULL AND file_hash!=''",[])?;
    }
    tx.execute_batch("DROP TABLE IF EXISTS community_summaries; DROP TABLE IF EXISTS flow_snapshots; DROP TABLE IF EXISTS risk_index; DROP INDEX IF EXISTS idx_nodes_qualified; DROP INDEX IF EXISTS idx_edges_source; DROP INDEX IF EXISTS idx_edges_target;")?;
    set_metadata(tx, "schema_version", "15")?;
    if quiet {
        return Ok(());
    }
    eprintln!("INFO: Schema version {old} -> 15: running migrations");
    for v in old + 1..=15 {
        eprintln!("INFO: Running migration v{v}");
        let line = match v {
            2 if steps.contains(&"signature") => "added 'signature' column to nodes",
            2 => continue,
            3 => "created flows and flow_memberships tables",
            4 => {
                if steps.contains(&"community_id") {
                    eprintln!("INFO: Migration v4: added 'community_id' column to nodes");
                }
                "created communities table"
            }
            5 => "created nodes_fts FTS5 virtual table",
            6 => "created summary tables (community_summaries, flow_snapshots, risk_index)",
            7 => "added compound edge indexes",
            8 => "created composite edge index",
            9 => "added edge confidence columns",
            10 => "added indexed nodes.symbol column",
            11 => "added indexed edges.target_resolution column",
            12 => "added nodes_fts_state index mirror",
            13 => "indexed nodes.docstring and nodes.name_tokens",
            14 => {
                eprintln!(
                    "INFO: Migration v14: invalidated {ruby} Ruby file(s) for re-keying on next update"
                );
                continue;
            }
            15 => "dropped unread tables and covered indexes",
            _ => unreachable!(),
        };
        eprintln!("INFO: Migration v{v}: {line}");
    }
    eprintln!(
        "INFO: Migrations complete, now at schema version 15\nINFO: Graph upgraded to schema v15. Run code-review-graph build once (and code-review-graph embed if you use embeddings): an update parses only changed files, so files this release can newly read stay out until then."
    );
    Ok(())
}

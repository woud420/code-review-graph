use anyhow::Result;
use rusqlite::Transaction;

pub(super) fn sync(
    tx: &Transaction<'_>,
    total: usize,
    full: bool,
) -> Result<Option<(usize, usize)>> {
    let state_size: i64 = tx.query_row("SELECT count(*) FROM nodes_fts_state", [], |r| r.get(0))?;
    if !full && (state_size != 0 || total == 0) {
        tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS affected(file TEXT PRIMARY KEY);
            CREATE TEMP TABLE fts_delta(id INTEGER PRIMARY KEY);
            INSERT INTO fts_delta SELECT n.id FROM nodes n LEFT JOIN nodes_fts_state s ON s.node_id=n.id
                WHERE s.node_id IS NULL OR n.file_path IN (SELECT file FROM affected)
                OR (n.name,n.qualified_name,n.file_path,n.signature,n.docstring,n.name_tokens)
                IS NOT (s.name,s.qualified_name,s.file_path,s.signature,s.docstring,s.name_tokens);
            INSERT OR IGNORE INTO fts_delta SELECT s.node_id FROM nodes_fts_state s LEFT JOIN nodes n ON n.id=s.node_id
                WHERE n.id IS NULL OR s.file_path IN (SELECT file FROM affected)")?;
        let changed: i64 = tx.query_row("SELECT count(*) FROM fts_delta", [], |r| r.get(0))?;
        if usize::try_from(changed)? < 500.max(total * 35 / 100) {
            tx.execute_batch("INSERT INTO nodes_fts(nodes_fts,rowid,name,qualified_name,file_path,signature,docstring,name_tokens)
                SELECT 'delete',s.* FROM nodes_fts_state s JOIN fts_delta d ON d.id=s.node_id;")?;
            let removed = tx.execute(
                "DELETE FROM nodes_fts_state WHERE node_id IN (SELECT id FROM fts_delta)",
                [],
            )?;
            let added = tx.execute("INSERT INTO nodes_fts_state SELECT n.id,n.name,n.qualified_name,n.file_path,n.signature,n.docstring,n.name_tokens
                FROM nodes n JOIN fts_delta d ON d.id=n.id", [])?;
            tx.execute_batch("INSERT INTO nodes_fts(rowid,name,qualified_name,file_path,signature,docstring,name_tokens)
                SELECT s.* FROM nodes_fts_state s JOIN fts_delta d ON s.node_id=d.id")?;
            return Ok(Some((removed, added)));
        }
    }
    tx.execute_batch("INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild');
        DELETE FROM nodes_fts_state;
        INSERT INTO nodes_fts_state(node_id,name,qualified_name,file_path,signature,docstring,name_tokens)
          SELECT id,name,qualified_name,file_path,signature,docstring,name_tokens FROM nodes;
        INSERT OR REPLACE INTO metadata VALUES('fts_state_synced','2')")?;
    Ok(None)
}

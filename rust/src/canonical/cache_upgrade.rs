use super::{INDEX_VERSION, index, postprocess::Level};
use crate::graph::Graph;
use anyhow::Result;
use rusqlite::Connection;
use std::fs;

#[test]
fn upgrade_rebuilds_matching_hashes_and_retries_after_a_parse_failure() -> Result<()> {
    for legacy_cache in [1, 2] {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let db = root.join(".code-review-graph/graph.db");
        let cache = db.with_extension("rust-cache");
        let source = root.join("example.py");
        let program = "def process(): pass\ndef main(): process()\n";
        fs::write(&source, program)?;
        index(&root, &db, true, Level::Full, false)?;
        let canonical = Connection::open(&db)?;
        canonical.execute(
            "UPDATE metadata SET value='1' WHERE key='rust_index_version'",
            [],
        )?;
        canonical.execute("DELETE FROM edges WHERE kind='CALLS'", [])?;
        let private = Connection::open(&cache)?;
        private.execute("UPDATE files SET facts='{}'", [])?;
        private.execute("DELETE FROM calls", [])?;
        if legacy_cache == 1 {
            private.execute("DROP TABLE call_sites", [])?;
        }
        private.pragma_update(None, "user_version", legacy_cache)?;
        fs::write(&source, b"\xff")?;
        assert!(index(&root, &db, false, Level::Full, false).is_err());
        assert_eq!(
            super::metadata(&canonical, "rust_index_version")?.as_deref(),
            Some("1")
        );
        assert_eq!(
            private.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
            legacy_cache
        );
        assert_eq!(Graph::open_existing(&cache)?.stats()?.edges, 0);
        fs::write(&source, program)?;
        assert!(index(&root, &db, false, Level::Full, false)?.full);
        assert_eq!(
            super::metadata(&canonical, "rust_index_version")?.as_deref(),
            Some(INDEX_VERSION)
        );
        assert_eq!(
            private.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
            3
        );
        assert_eq!(
            Graph::open_existing(&cache)?.callees("example.py::main")?[0].name,
            "process"
        );
        assert_eq!(
            canonical.query_row(
                "SELECT count(*) FROM edges WHERE kind='CALLS' AND target_resolution='direct'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            1
        );
        assert_eq!(
            index(&root, &db, false, Level::Full, false)?.files_parsed,
            0
        );
    }
    Ok(())
}

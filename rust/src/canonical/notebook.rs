use super::imports::ImportDetails;
use anyhow::Result;
use rusqlite::{Transaction, params};
use std::{collections::BTreeSet, path::Path};

// The Python indexer enriches notebook imports at full build and after a .py change.
pub(super) fn resolve_imports(
    tx: &Transaction,
    root: &Path,
    paths: &BTreeSet<String>,
) -> Result<()> {
    let mut select=tx.prepare("SELECT e.id,e.file_path,e.target_qualified,e.line FROM edges e JOIN nodes n ON n.qualified_name=e.file_path
        WHERE e.kind='IMPORTS_FROM' AND e.extra='{}' AND n.language='python' AND (e.file_path GLOB '*.ipynb' OR json_extract(n.extra,'$.notebook_format')='databricks_py')")?;
    let mut update = tx.prepare("UPDATE edges SET target_qualified=?,extra=? WHERE id=?")?;
    let mut rows = select.query([])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let path: String = row.get(1)?;
        let raw: String = row.get(2)?;
        let line = usize::try_from(row.get::<_, i64>(3)?)?;
        let Some(relative) = Path::new(&path)
            .strip_prefix(root)
            .ok()
            .and_then(Path::to_str)
        else {
            continue;
        };
        let (target, extra) =
            ImportDetails::unscoped(line).resolve(root, relative, &raw, raw.clone(), line, paths);
        update.execute(params![target, extra.to_string(), id])?;
    }
    Ok(())
}

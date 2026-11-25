mod diff;
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Component, PathBuf};
use std::time::{Duration, Instant};

struct Preview {
    created: Instant,
    root: PathBuf,
    edits: BTreeMap<String, Vec<Value>>,
    followed: BTreeMap<String, PathBuf>,
}
#[derive(Default)]
pub(in crate::cli::reports) struct Pending(Mutex<BTreeMap<String, Preview>>);
impl Pending {
    pub(in crate::cli::reports) fn remember(
        &self,
        conn: &Connection,
        id: &str,
        root: &Path,
        edits: &[Value],
    ) -> Result<()> {
        let approved = followed(conn)?;
        let mut preview = Preview {
            created: Instant::now(),
            root: root.canonicalize()?,
            edits: BTreeMap::new(),
            followed: BTreeMap::new(),
        };
        for edit in edits {
            let path = text(edit, "file");
            preview
                .edits
                .entry(path.into())
                .or_default()
                .push(edit.clone());
            if approved.contains(path)
                && let Ok(target) = Path::new(path).canonicalize()
            {
                preview.followed.insert(path.into(), target);
            }
        }
        let mut pending = self.0.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|_, p| p.created.elapsed() <= Duration::from_secs(600));
        pending.insert(id.into(), preview);
        Ok(())
    }
    pub(in crate::cli::reports) fn apply(&self, root: &Path, args: &Value) -> Result<Value> {
        let id = text(args, "refactor_id");
        let dry = args["dry_run"].as_bool().unwrap_or(false);
        let max = positive(args, "max_diff_files", 25, 150)?;
        let mut pending = self.0.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|_, p| p.created.elapsed() <= Duration::from_secs(600));
        let Some(p) = pending.get(id) else {
            return Ok(
                json!({"status":"error","error":format!("Refactor '{id}' not found or expired.")}),
            );
        };
        let root = root.canonicalize()?;
        let mut paths = BTreeMap::new();
        for file in p.edits.keys() {
            let path = Path::new(file);
            let target = path.canonicalize().ok();
            // Recheck both the lexical path and the resolved destination on every use of a preview.
            if !path.starts_with(&root)
                || p.root != root
                || path.components().any(|c| c == Component::ParentDir)
                || target
                    .as_ref()
                    .is_some_and(|r| !r.starts_with(&root) && p.followed.get(file) != Some(r))
            {
                return Ok(
                    json!({"status":"error","error":format!("Edit path '{file}' is outside repo root.")}),
                );
            }
            paths.insert(file, target);
        }
        let mut diffs = serde_json::Map::new();
        let mut touched = Vec::new();
        let (mut applied, mut skipped) = (0, 0);
        for (file, edits) in &p.edits {
            let Some(path) = paths[file].as_ref() else {
                skipped += edits.len();
                continue;
            };
            if !path.is_file() {
                skipped += edits.len();
                continue;
            }
            let Ok(original) = fs::read_to_string(path) else {
                skipped += edits.len();
                continue;
            };
            let without_bom = original.strip_prefix('\u{feff}').unwrap_or(&original);
            let (changed, count, missed) = replace(without_bom, edits);
            skipped += missed;
            if count == 0 {
                continue;
            }
            if dry {
                if diffs.len() < max {
                    diffs.insert(
                        file.clone(),
                        json!(diff::unified(file, without_bom, &changed)),
                    );
                }
            } else {
                let mut replacement = Vec::new();
                if original.starts_with('\u{feff}') {
                    replacement.extend_from_slice(&[0xef, 0xbb, 0xbf]);
                }
                replacement.extend_from_slice(changed.as_bytes());
                if let Err(err) =
                    write_if_current(Path::new(file), path, original.as_bytes(), &replacement)
                {
                    eprintln!("ERROR: apply_refactor: could not write {file}: {err}");
                    skipped += count;
                    continue;
                }
            }
            applied += count;
            touched.push(file.clone());
        }
        let mut result = if dry {
            json!({"status":"ok","dry_run":true,"applied":0,"edits_applied":applied,"would_modify":touched,"files_modified":[],"diffs":diffs})
        } else {
            pending.remove(id);
            json!({"status":"ok","applied":applied,"files_modified":touched,"edits_applied":applied})
        };
        if skipped > 0 {
            result["edits_skipped"] = json!(skipped);
        }
        if dry && touched.len() > max {
            result["diffs_total"] = json!(touched.len());
            result["diffs_truncated"] = json!(true);
        }
        Ok(result)
    }
}
pub(super) fn followed(conn: &Connection) -> Result<BTreeSet<String>> {
    let raw = crate::cli::store::metadata(conn, "symlink_followed_files")?;
    Ok(raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default())
}
fn write_if_current(lexical: &Path, target: &Path, original: &[u8], data: &[u8]) -> Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing file parent"))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.as_file()
        .set_permissions(fs::metadata(target)?.permissions())?;
    temp.write_all(data)?;
    temp.flush()?;
    ensure!(
        lexical.canonicalize()? == target && fs::read(target)? == original,
        "source changed during refactoring"
    );
    temp.persist(target)?;
    Ok(())
}
fn replace(source: &str, edits: &[Value]) -> (String, usize, usize) {
    let mut lines: Vec<_> = source.split('\n').map(str::to_owned).collect();
    let mut claimed = BTreeMap::new();
    let mut skipped = 0;
    let mut ordered: Vec<_> = edits.iter().collect();
    ordered.sort_by_key(|e| e["column"].is_null());
    let word = |ch: char| ch.is_alphanumeric() || matches!(ch, '_' | '$');
    for e in ordered {
        let line = e["line"].as_u64().unwrap_or(0) as usize;
        let old = text(e, "old");
        let new = text(e, "new");
        let Some(v) = line
            .checked_sub(1)
            .and_then(|i| lines.get(i))
            .filter(|_| !old.is_empty())
        else {
            skipped += 1;
            continue;
        };
        let column = e["column"]
            .as_u64()
            .and_then(|c| c.checked_sub(1))
            .map(|c| c as usize);
        let at = v.match_indices(old).map(|(i, _)| i).find(|&i| {
            if column.is_some_and(|c| v[..i].chars().count() != c)
                || column.is_none() && claimed.contains_key(&(line, i))
            {
                return false;
            }
            let before = v[..i].chars().next_back();
            let after = v[i + old.len()..].chars().next();
            !(old.starts_with(word) && before.is_some_and(word)
                || old.ends_with(word) && after.is_some_and(word))
        });
        if let Some(i) = at {
            claimed.entry((line, i)).or_insert((old, new));
        } else {
            skipped += 1;
        }
    }
    let count = claimed.len();
    for ((line, i), (old, new)) in claimed.into_iter().rev() {
        lines[line - 1].replace_range(i..i + old.len(), new);
    }
    (lines.join("\n"), count, skipped)
}

use super::*;

fn fit(s: &str, cap: usize) -> String {
    let safe = clean(s).split_whitespace().collect::<Vec<_>>().join(" ");
    if safe.chars().count() <= cap {
        safe
    } else {
        format!(
            "{}~",
            safe.chars().take(cap.saturating_sub(1)).collect::<String>()
        )
    }
}

fn message(prefix: &str, target: &str, suffix: &str) -> String {
    let cap = 140usize.saturating_sub(prefix.len() + suffix.len());
    let shortened = if target.chars().count() > cap {
        target
            .rsplit("::")
            .next()
            .filter(|s| s.chars().count() <= cap)
            .unwrap_or(target)
    } else {
        target
    };
    format!("{prefix}{}{suffix}", fit(shortened, cap))
}

fn untracked(conn: &Connection, root: &Path) -> Result<Option<usize>> {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            ".",
        ])
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    if output.stdout.is_empty() {
        return Ok(Some(0));
    }
    let files = conn
        .prepare("SELECT file_path FROM nodes WHERE kind='File'")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let indexed: HashSet<_> = files.iter().map(|s| root.join(s)).collect();
    let extensions: HashSet<_> = indexed.iter().filter_map(|s| s.extension()).collect();
    let mut ignore = ignore::gitignore::GitignoreBuilder::new(root);
    ignore.add(root.join(".code-review-graphignore"));
    let ignore = ignore.build()?;
    let raw = String::from_utf8_lossy(&output.stdout);
    let count = raw
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(Path::new)
        .filter(|s| {
            s.extension().is_some_and(|e| extensions.contains(e))
                && !indexed.contains(&root.join(s))
                && !ignore.matched_path_or_any_parents(s, false).is_ignore()
                && !s.iter().any(|n| {
                    [
                        "node_modules",
                        "target",
                        "dist",
                        "build",
                        ".code-review-graph",
                        ".venv",
                        "vendor",
                        "__pycache__",
                    ]
                    .iter()
                    .any(|known| n == *known)
                })
        })
        .count();
    Ok(Some(count))
}

pub(crate) fn empty(
    conn: &Connection,
    root: &Path,
    target: &str,
    node: Option<&Value>,
    pattern: &str,
) -> Result<String> {
    if node.is_none()
        && conn.query_row("SELECT NOT EXISTS(SELECT 1 FROM nodes)", [], |r| {
            r.get::<_, bool>(0)
        })?
    {
        return Ok("graph is empty: nothing is indexed, so this 0 says nothing about the code; run `code-review-graph build`".into());
    }
    let built = store::metadata(conn, "git_head_sha")?.unwrap_or_default();
    let live = if built.is_empty() {
        String::new()
    } else {
        store::git(root, &["rev-parse", "HEAD"])
    };
    if !live.is_empty() && live != built {
        return Ok(if node.is_some() {
            "graph is stale: built at an older commit than HEAD, so this 0 may be out of date; run `code-review-graph update`".into()
        } else {
            message(
                "graph is stale: no node matching '",
                target,
                "'; the graph predates HEAD, so run `code-review-graph update` first",
            )
        });
    }
    let mut verified = !live.is_empty();
    let mut check_untracked = true;
    if let Some(n) = node {
        if let Some(date) = store::metadata(conn, "last_updated")?.filter(|s| !s.is_empty()) {
            let path = root.join(text(n, "file_path"));
            let time = path
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok());
            let epoch:Option<f64>=conn.query_row("SELECT unixepoch(?1,CASE WHEN substr(?1,-1)='Z' OR instr(substr(?1,12),'+') OR instr(substr(?1,12),'-') THEN '+0 seconds' ELSE 'utc' END,'subsec')",[date],|r|r.get(0))?;
            if let (Some(time), Some(epoch)) = (time, epoch) {
                if time.as_secs_f64() > epoch {
                    return Ok(message(
                        "graph is stale: ",
                        path.file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or_default(),
                        " changed after the last build; run `code-review-graph update`",
                    ));
                }
            } else {
                check_untracked = false;
                verified = false;
            }
        } else {
            verified = false;
        }
    }
    if check_untracked {
        match untracked(conn, root).unwrap_or(None) {
            Some(n) if n > 0 => {
                return Ok(message(
                    "",
                    &n.to_string(),
                    " source file(s) untracked by git, absent from the index, so this 0 may be short; `git add` them, then `code-review-graph update`",
                ));
            }
            None => verified = false,
            _ => {}
        }
    }
    if let Some(n) = node {
        if let Some(gap) = terms::language_gap(text(n, "language"), pattern) {
            return Ok(fit(gap, 140));
        }
        let tail = if verified {
            "' is indexed and the graph is current, so this 0 is a real absence"
        } else {
            "' is indexed and no such edge is recorded; graph currency unverified"
        };
        Ok(message("'", target, tail))
    } else {
        Ok(message(
            "target not indexed: no node matching '",
            target,
            "', so this 0 is not evidence that none exist",
        ))
    }
}

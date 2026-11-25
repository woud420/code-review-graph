use super::{build, changes, store};
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fmt::Write,
    io::Read,
    path::{Component, Path, PathBuf},
};

mod enrich;
pub(super) use enrich::search_hook;

fn payload() -> Option<Value> {
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value.is_object().then_some(value)
}

fn cwd(event: &Value) -> Option<PathBuf> {
    let path = Path::new(event.get("cwd")?.as_str()?);
    (path.is_absolute() && path.is_dir())
        .then(|| path.canonicalize().ok())
        .flatten()
}

fn git_root(dir: &Path, flag: &str) -> Option<PathBuf> {
    let (ok, bytes) =
        changes::git_timed(dir, &["rev-parse", flag], "CRG_DISCOVERY_TIMEOUT", 5.).ok()?;
    let path = std::str::from_utf8(&bytes).ok()?.trim();
    (ok && !path.is_empty())
        .then(|| Path::new(path).canonicalize().ok())
        .flatten()
}

fn resolve(path: &Path) -> Option<PathBuf> {
    let mut result = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            part => {
                result.push(part);
                if result.symlink_metadata().is_ok() {
                    result = result.canonicalize().ok()?;
                }
            }
        }
    }
    Some(result)
}

fn edited(event: &Value, cwd: Option<&Path>) -> Option<PathBuf> {
    let input = event.get("tool_input")?.as_object()?;
    let value = ["file_path", "notebook_path"]
        .iter()
        .find_map(|k| input.get(*k)?.as_str().filter(|s| !s.trim().is_empty()))?;
    let p = Path::new(value);
    if p.is_absolute() {
        resolve(p)
    } else {
        resolve(&cwd?.join(p))
    }
}

fn recorded_root(db: &Path) -> Option<PathBuf> {
    // A read-only connection to a checkpointed WAL database can still create sidecars.
    let path = db.canonicalize().ok()?;
    let mut uri = String::from("file:");
    for byte in path.as_os_str().as_encoded_bytes() {
        match *byte {
            b'%' | b'?' | b'#' | 0x80..=0xff => write!(uri, "%{byte:02x}").ok()?,
            _ => uri.push(*byte as char),
        }
    }
    uri.push_str("?mode=ro");
    let active = ["-wal", "-shm"].iter().any(|s| {
        let mut path = db.as_os_str().to_owned();
        path.push(s);
        Path::new(&path).exists()
    });
    if !active {
        uri.push_str("&immutable=1");
    }
    let conn = Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .ok()?;
    conn.busy_timeout(std::time::Duration::from_secs(1)).ok()?;
    let owner = store::metadata(&conn, "repo_root").ok()??;
    Path::new(&owner).canonicalize().ok()
}

fn has_graph(root: &Path) -> bool {
    let db = store::database(root, None);
    let default = root.join(".code-review-graph");
    if db.parent().is_some_and(|p| resolve(p) == resolve(&default)) {
        return db.is_file() || root.join(".code-review-graph.db").is_file();
    }
    db.is_file() && recorded_root(&db).as_deref() == Some(root)
}

pub(super) fn update(existing_only: bool) -> Result<()> {
    let Some(event) = payload() else {
        return Ok(());
    };
    let cwd = cwd(&event);
    let session = cwd.as_deref().and_then(|d| git_root(d, "--show-toplevel"));
    let target = if let Some(file) = edited(&event, cwd.as_deref()) {
        let mut target = file
            .parent()
            .and_then(|p| p.ancestors().find(|p| p.is_dir()))
            .and_then(|p| git_root(p, "--show-toplevel"));
        let mut seen = BTreeSet::new();
        while let Some(repo) = &target {
            if !seen.insert(repo.clone()) {
                return Ok(());
            }
            if (!existing_only && session.as_ref() == Some(repo)) || has_graph(repo) {
                break;
            }
            target = git_root(repo, "--show-superproject-working-tree");
        }
        target
    } else {
        session.filter(|r| !existing_only || has_graph(r))
    };
    if let Some(repo) = target {
        let result = build::run(&repo, &json!({"postprocess":"minimal"})).context("Error")?;
        if result["status"] == "error" {
            anyhow::bail!(
                "Error: {}",
                result["summary"].as_str().unwrap_or("update failed")
            );
        }
    }
    Ok(())
}

pub(super) fn status() -> Result<()> {
    let Some(root) = payload()
        .as_ref()
        .and_then(cwd)
        .and_then(|p| git_root(&p, "--show-toplevel"))
    else {
        return Ok(());
    };
    if let Err(e) = store::open(&store::database(&root, None))
        .and_then(|conn| store::status(&root, &conn, false, false))
    {
        eprintln!("{e:#}");
    }
    Ok(())
}

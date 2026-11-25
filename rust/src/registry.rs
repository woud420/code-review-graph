use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

fn location() -> Result<PathBuf> {
    if let Ok(value) = std::env::var("CRG_HOME")
        && !value.trim().is_empty()
    {
        return Ok(absolute(Path::new(value.trim()))?.join("registry.json"));
    }
    Ok(std::env::home_dir()
        .context("cannot determine the home directory")?
        .join(".code-review-graph/registry.json"))
}

fn load(path: &Path) -> Result<Vec<Value>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) => Ok(v["repos"].as_array().cloned().unwrap_or_default()),
        Err(_) => {
            eprintln!(
                "WARNING: Invalid registry file, starting fresh: {}",
                path.display()
            );
            Ok(Vec::new())
        }
    }
}

pub fn list() -> Result<Vec<Value>> {
    load(&location()?)
}

fn modify(mut edit: impl FnMut(&mut Vec<Value>) -> Result<bool>) -> Result<()> {
    let file = location()?;
    let dir = file.parent().context("missing registry directory")?;
    fs::create_dir_all(dir)?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("registry.lock"))?;
    lock.lock()?;
    let mut entries = load(&file)?;
    if edit(&mut entries)? {
        let pending = dir.join(format!("registry.{}.tmp", std::process::id()));
        let mut writer = File::options()
            .create_new(true)
            .write(true)
            .open(&pending)?;
        let result = (|| -> Result<()> {
            serde_json::to_writer_pretty(&mut writer, &json!({"repos":entries}))?;
            writer.write_all(b"\n")?;
            writer.sync_all()?;
            fs::rename(&pending, &file)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&pending);
        }
        result?;
    }
    Ok(())
}

pub fn register_repo(path: &Path, alias: Option<String>) -> Result<Value> {
    let path = absolute(path)?;
    ensure!(
        path.is_dir(),
        "ERROR: Path is not a directory: {}",
        path.display()
    );
    ensure!(
        [".git", ".svn", ".code-review-graph"]
            .iter()
            .any(|s| path.join(s).exists()),
        "ERROR: Path does not look like a repository (no .git, .svn, or .code-review-graph): {}",
        path.display()
    );
    let path = path.to_string_lossy();
    let mut registered = Value::Null;
    modify(|entries| {
        let index = entries
            .iter()
            .position(|v| v["path"].as_str() == Some(path.as_ref()))
            .unwrap_or_else(|| {
                entries.push(json!({"path":path}));
                entries.len() - 1
            });
        let entry = &mut entries[index];
        if let Some(alias) = alias.as_ref().filter(|s| !s.is_empty()) {
            entry["alias"] = json!(alias);
        }
        registered = entry.clone();
        Ok(true)
    })?;
    Ok(registered)
}

pub fn set_data_dir(root: &Path, dir: &Path) -> Result<()> {
    let path = absolute(root)?;
    let dir = absolute(dir)?;
    fs::create_dir_all(&dir)?;
    modify(|entries| {
        if let Some(found) = entries
            .iter_mut()
            .find(|v| v["path"].as_str() == path.to_str())
        {
            found["data_dir"] = json!(dir);
        } else {
            entries.push(json!({"path":path,"data_dir":dir}));
        }
        Ok(true)
    })
}

pub fn unregister_repo(raw: &str) -> Result<bool> {
    let path = absolute(Path::new(raw))?;
    let mut found = false;
    modify(|entries| {
        let before = entries.len();
        entries.retain(|v| v["alias"].as_str() != Some(raw) && v["path"].as_str() != path.to_str());
        found = entries.len() < before;
        Ok(found)
    })?;
    Ok(found)
}

pub fn absolute(path: &Path) -> Result<PathBuf> {
    let expanded = if let Ok(rest) = path.strip_prefix("~") {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(rest)
    } else {
        path.to_owned()
    };
    Ok(expanded
        .canonicalize()
        .unwrap_or(std::path::absolute(expanded)?))
}

macro_rules! daemon_log { ($($arg:tt)*) => { super::log(format_args!($($arg)*)) }; }
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashSet;
use std::env;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

pub(super) fn home() -> Result<PathBuf> {
    if let Some(value) = env::var_os("CRG_HOME").filter(|s| !s.to_string_lossy().trim().is_empty())
    {
        return expand(Path::new(value.to_string_lossy().trim()));
    }
    Ok(env::home_dir()
        .context("cannot determine user home")?
        .join(".code-review-graph"))
}

fn expand(path: &Path) -> Result<PathBuf> {
    if let Ok(rest) = path.strip_prefix("~") {
        return Ok(env::home_dir()
            .context("cannot determine user home")?
            .join(rest));
    }
    Ok(path.to_owned())
}

fn resolve(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(expand(path)?)?;
    if let Ok(path) = absolute.canonicalize() {
        return Ok(path);
    }
    let mut result = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::ParentDir => {
                result.pop();
            }
            _ => result.push(component),
        }
        if let Ok(real) = result.canonicalize() {
            result = real;
        }
    }
    Ok(result)
}

fn has_marker(path: &Path) -> bool {
    [".git", ".svn", ".code-review-graph"]
        .iter()
        .any(|name| path.join(name).exists())
}

#[derive(Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub(super) struct Repo {
    pub path: PathBuf,
    pub alias: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Settings {
    session_name: Option<String>,
    log_dir: Option<PathBuf>,
    poll_interval: Option<i64>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Raw {
    daemon: Settings,
    repos: Vec<Repo>,
}

pub(super) struct Config {
    pub name: String,
    pub logs: PathBuf,
    pub poll: i64,
    pub repos: Vec<Repo>,
}

pub(super) fn load(home: &Path) -> Result<Config> {
    let path = home.join("watch.toml");
    let raw: Raw = match fs::read_to_string(&path) {
        Ok(s) => {
            toml::from_str(&s).with_context(|| format!("invalid config {}", path.display()))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            daemon_log!(
                "INFO: Config file not found at {} — using defaults",
                path.display()
            );
            Raw::default()
        }
        Err(e) => return Err(e.into()),
    };
    let mut repos = Vec::new();
    let mut seen = HashSet::new();
    for mut repo in raw.repos {
        if repo.path.as_os_str().is_empty() {
            daemon_log!("WARNING: Skipping repo entry with empty path");
            continue;
        }
        repo.path = resolve(&repo.path)?;
        if !repo.path.is_dir() {
            daemon_log!(
                "WARNING: Skipping repo {} — directory does not exist",
                repo.path.display()
            );
            continue;
        }
        if !has_marker(&repo.path) {
            daemon_log!(
                "WARNING: Skipping repo {} — no .git, .svn, or .code-review-graph directory found",
                repo.path.display()
            );
            continue;
        }
        if repo.alias.is_empty() {
            repo.alias = basename(&repo.path);
        }
        if !seen.insert(repo.alias.clone()) {
            daemon_log!(
                "WARNING: Skipping duplicate alias '{}' for repo {}",
                repo.alias,
                repo.path.display()
            );
            continue;
        }
        repos.push(repo);
    }
    Ok(Config {
        name: raw
            .daemon
            .session_name
            .unwrap_or_else(|| "crg-watch".into()),
        logs: raw.daemon.log_dir.unwrap_or_else(|| home.join("logs")),
        poll: raw.daemon.poll_interval.unwrap_or(2),
        repos,
    })
}

fn quote(s: &str) -> String {
    let mut buf = String::from("\"");
    for ch in s.chars() {
        match ch {
            '\\' => buf.push_str("\\\\"),
            '"' => buf.push_str("\\\""),
            '\n' => buf.push_str("\\n"),
            '\t' => buf.push_str("\\t"),
            '\r' => buf.push_str("\\r"),
            '\x08' => buf.push_str("\\b"),
            '\x0c' => buf.push_str("\\f"),
            c if c < ' ' || c == '\x7f' => write!(buf, "\\u{:04X}", c as u32).unwrap(),
            c => buf.push(c),
        }
    }
    buf.push('"');
    buf
}

fn basename(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

pub(super) fn atomic_write(path: &Path, value: impl AsRef<[u8]>) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let mut handle = File::options().write(true).create_new(true).open(&tmp)?;
    use std::io::Write;
    let write = handle
        .write_all(value.as_ref())
        .and_then(|()| fs::rename(&tmp, path));
    if write.is_err() {
        let _ = fs::remove_file(tmp);
    }
    Ok(write?)
}

fn save(home: &Path, c: &Config) -> Result<()> {
    let path = home.join("watch.toml");
    let mut out = format!(
        "[daemon]\nsession_name = {}\nlog_dir = {}\npoll_interval = {}\n",
        quote(&c.name),
        quote(&c.logs.to_string_lossy()),
        c.poll
    );
    for repo in &c.repos {
        writeln!(
            out,
            "\n[[repos]]\npath = {}\nalias = {}",
            quote(&repo.path.to_string_lossy()),
            quote(&repo.alias)
        )?;
    }
    atomic_write(&path, out)?;
    daemon_log!("INFO: Config saved to {}", path.display());
    Ok(())
}

pub(super) fn lock(home: &Path, name: &str) -> Result<File> {
    fs::create_dir_all(home)?;
    Ok(File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join(name))?)
}

pub(super) fn add(home: &Path, input: &Path, alias: Option<String>) -> Result<()> {
    let path = resolve(input)?;
    if !path.is_dir() {
        bail!("Not a directory: {}", path.display());
    }
    if !has_marker(&path) {
        bail!(
            "No .git, .svn, or .code-review-graph directory in {}",
            path.display()
        );
    }
    let alias = alias.filter(|name| !name.is_empty());
    let effective = alias.clone().unwrap_or_else(|| basename(&path));
    let guard = lock(home, "watch-config.lock")?;
    guard.lock()?;
    let mut c = load(home)?;
    let mut duplicate = false;
    for old in &c.repos {
        if old.path == path {
            daemon_log!(
                "WARNING: Repo {} is already configured — skipping",
                path.display()
            );
            duplicate = true;
            break;
        }
        if old.alias == effective {
            bail!(
                "Alias '{effective}' is already in use by {}",
                old.path.display()
            );
        }
    }
    if !duplicate {
        c.repos.push(Repo {
            path,
            alias: effective,
        });
        save(home, &c)?;
    }
    let displayed =
        alias.unwrap_or_else(|| basename(&std::path::absolute(input).unwrap_or_default()));
    println!(
        "Added repository: {} (alias: {})",
        input.display(),
        displayed
    );
    changed(home)?;
    Ok(())
}

pub(super) fn remove(home: &Path, value: &str) -> Result<bool> {
    let guard = lock(home, "watch-config.lock")?;
    guard.lock()?;
    let _before = load(home)?;
    let mut c = load(home)?;
    let count = c.repos.len();
    let path = resolve(Path::new(value))?;
    c.repos.retain(|r| r.path != path && r.alias != value);
    if count == c.repos.len() {
        daemon_log!("WARNING: No repo matching '{value}' found in config — nothing removed");
        println!("No repository matching '{value}' found in config.");
        return Ok(false);
    }
    save(home, &c)?;
    println!("Removed repository: {value}");
    changed(home)?;
    Ok(true)
}

fn changed(home: &Path) -> Result<()> {
    if super::lifecycle::running(home)?.is_some() {
        println!("Daemon will pick up the change automatically.");
    }
    Ok(())
}

pub(super) fn log_file(config: &Config, alias: Option<&str>) -> Result<PathBuf> {
    let alias = alias.filter(|s| !s.is_empty()).unwrap_or("daemon");
    if Path::new(alias).file_name() != Some(std::ffi::OsStr::new(alias)) {
        bail!("Invalid log alias '{alias}'");
    }
    Ok(config.logs.join(format!("{alias}.log")))
}

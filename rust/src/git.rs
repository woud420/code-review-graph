use anyhow::{Context, Result, bail};
use std::{
    collections::HashSet,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub fn timed(root: &Path, args: &[&str], setting: &str, default: f64) -> Result<(bool, Vec<u8>)> {
    let (code, bytes) = output(root, args, setting, default)?;
    Ok((code == 0, bytes))
}

fn output(root: &Path, args: &[&str], setting: &str, default: f64) -> Result<(i32, Vec<u8>)> {
    let configured = |key| {
        std::env::var(key)
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|s| s.is_finite() && *s >= 0.)
    };
    let (seconds, knob) = if setting == "CRG_DISCOVERY_TIMEOUT"
        && configured(setting).is_none()
        && configured("CRG_GIT_TIMEOUT").is_some()
    {
        (
            configured("CRG_GIT_TIMEOUT").unwrap_or(default),
            "CRG_GIT_TIMEOUT",
        )
    } else {
        (configured(setting).unwrap_or(default), setting)
    };
    if seconds == 0. {
        bail!(
            "could not determine the changes: git timed out after 0s. Raise {knob}, or re-run when the repository is not busy."
        );
    }
    let mut child=Command::new("git").current_dir(root).args(args).stdin(Stdio::null()).stderr(Stdio::null()).stdout(Stdio::piped()).spawn().context("could not determine the changes: git could not be run. Install git and make sure it is on PATH.")?;
    let mut stdout = child.stdout.take().context("git output unavailable")?;
    let read = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let start = Instant::now();
    loop {
        if let Some(exit) = child.try_wait()? {
            let bytes = read
                .join()
                .map_err(|_| anyhow::anyhow!("git output unavailable"))??;
            return Ok((exit.code().unwrap_or(-1), bytes));
        }
        if start.elapsed().as_secs_f64() >= seconds {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "could not determine the changes: git timed out after {seconds}s. Raise {knob}, or re-run when the repository is not busy."
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
pub enum Scope {
    Review,
    Update { authoritative: bool },
}

pub fn changed_paths(root: &Path, base: &str, scope: Scope) -> Result<Vec<String>> {
    let (setting, seconds) = match scope {
        Scope::Review => ("CRG_DISCOVERY_TIMEOUT", 3.),
        Scope::Update { .. } => ("CRG_GIT_TIMEOUT", 30.),
    };
    let authoritative = matches!(
        scope,
        Scope::Update {
            authoritative: true
        }
    );
    let run = |args: &[&str]| timed(root, args, setting, seconds);
    let mut paths = Vec::new();
    let diff = output(
        root,
        &["diff", "--relative", "--name-status", "-z", base, "--", "."],
        setting,
        seconds,
    );
    let (code, bytes) = if authoritative {
        diff.map_err(|_| anyhow::anyhow!("git change discovery failed"))?
    } else {
        diff?
    };
    if authoritative && code != 0 {
        bail!("git diff failed while discovering changed files (rc={code})");
    }
    if code == 0 {
        let mut fields = bytes.split(|&b| b == 0).filter(|b| !b.is_empty());
        while let Some(status) = fields.next() {
            let count = if matches!(status.first(), Some(b'R' | b'C')) {
                2
            } else {
                1
            };
            for v in fields.by_ref().take(count) {
                if let Ok(p) = std::str::from_utf8(v) {
                    paths.push(p.to_owned());
                }
            }
        }
    }
    if !paths.is_empty() {
        let (_, unknown) = run(&["ls-files", "--others", "--exclude-standard", "-z"])?;
        paths.extend(
            unknown
                .split(|&b| b == 0)
                .filter(|v| !v.is_empty())
                .filter_map(|b| std::str::from_utf8(b).ok().map(str::to_owned)),
        );
    }
    if paths.is_empty() {
        let (ok, status) = run(&[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            ".",
        ])?;
        if !ok {
            return Ok(Vec::new());
        }
        let (_, bytes) = run(&["rev-parse", "--show-prefix"])?;
        let prefix = String::from_utf8_lossy(&bytes).trim().to_owned();
        let mut fields = status.split(|&b| b == 0);
        while let Some(row) = fields.next() {
            if row.len() <= 3 {
                continue;
            }
            if let Ok(name) = std::str::from_utf8(&row[3..]) {
                paths.push(name.strip_prefix(&prefix).unwrap_or(name).to_owned());
            }
            if row[..2].iter().any(|b| matches!(b, b'R' | b'C')) {
                fields.next();
            }
        }
    }
    let mut seen = HashSet::new();
    paths.retain(|p| seen.insert(p.clone()));
    Ok(paths)
}

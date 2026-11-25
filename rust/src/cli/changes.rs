use anyhow::{Context, Result, bail};
use std::{
    collections::HashSet,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn git(root: &Path, args: &[&str]) -> Result<(bool, Vec<u8>)> {
    let seconds = std::env::var("CRG_DISCOVERY_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s > 0.)
        .unwrap_or(3.);
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
            return Ok((exit.success(), bytes));
        }
        if start.elapsed().as_secs_f64() >= seconds {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "could not determine the changes: git timed out after {seconds}s. Raise CRG_DISCOVERY_TIMEOUT, or re-run when the repository is not busy."
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn scalar(root: &Path, args: &[&str]) -> Result<String> {
    let (ok, bytes) = git(root, args)?;
    Ok(if ok {
        String::from_utf8_lossy(&bytes).trim().to_owned()
    } else {
        String::new()
    })
}
pub(super) fn resolve_base(root: &Path, base: &str) -> Result<String> {
    let unknown = || {
        anyhow::anyhow!(
            "base '{base}' is not a known revision. Check it with `git branch -a`, or run `git fetch origin <branch>` in a shallow clone."
        )
    };
    if base.starts_with('-') || !regex::Regex::new(r"^[A-Za-z0-9_.~^/@{}-]+$")?.is_match(base) {
        return Err(unknown());
    }
    let mut resolved = base.to_owned();
    let symbolic = scalar(
        root,
        &["rev-parse", "--symbolic-full-name", "--verify", base],
    )?;
    if symbolic.starts_with("refs/heads/") || symbolic.starts_with("refs/remotes/") {
        let ancestor = scalar(root, &["merge-base", base, "HEAD"])?;
        if !ancestor.is_empty() {
            resolved = ancestor;
        }
    }
    let has_git = root.ancestors().any(|d| d.join(".git").exists());
    if has_git {
        for side in resolved
            .split("..")
            .map(|s| s.trim_start_matches('.'))
            .filter(|s| !s.is_empty())
        {
            if side.starts_with('-') {
                return Err(unknown());
            }
            if scalar(
                root,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{side}^{{commit}}"),
                ],
            )?
            .is_empty()
            {
                if base != "HEAD~1" {
                    return Err(unknown());
                }
                resolved = if !scalar(root, &["rev-parse", "--verify", "HEAD"])?.is_empty() {
                    "HEAD".into()
                } else {
                    scalar(root, &["hash-object", "-t", "tree", "--stdin"])?
                };
                break;
            }
        }
    }
    Ok(resolved)
}

pub(super) fn discover(root: &Path, base: &str) -> Result<Vec<String>> {
    let resolved = resolve_base(root, base)?;
    let mut paths = Vec::new();
    let (ok, bytes) = git(
        root,
        &[
            "diff",
            "--relative",
            "--name-status",
            "-z",
            &resolved,
            "--",
            ".",
        ],
    )?;
    if ok {
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
        let (_, unknown) = git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
        paths.extend(
            unknown
                .split(|&b| b == 0)
                .filter(|v| !v.is_empty())
                .filter_map(|b| std::str::from_utf8(b).ok().map(str::to_owned)),
        );
    }
    if paths.is_empty() {
        let (ok, status) = git(
            root,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--",
                ".",
            ],
        )?;
        if !ok {
            return Ok(Vec::new());
        }
        let prefix = scalar(root, &["rev-parse", "--show-prefix"])?;
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

pub(super) fn paths(root: &Path, args: &serde_json::Value) -> Result<Vec<String>> {
    if let Some(value) = args.get("changed_files").filter(|v| !v.is_null()) {
        return Ok(serde_json::from_value(value.clone())?);
    }
    discover(root, args["base"].as_str().unwrap_or("HEAD~1"))
}

pub(super) fn hunks(
    root: &Path,
    base: &str,
) -> Result<std::collections::HashMap<String, Vec<(i64, i64)>>> {
    let (_, diff) = git(root, &["diff", "--unified=0", base, "--"])?;
    let mut out = std::collections::HashMap::new();
    let mut file = String::new();
    let header = regex::Regex::new(r"^@@ -[0-9]+(?:,[0-9]+)? \+([0-9]+)(?:,([0-9]+))? @@")?;
    for line in String::from_utf8_lossy(&diff).lines() {
        if line.starts_with("diff --git ") {
            file.clear();
        } else if let Some(path) = line.strip_prefix("+++ b/") {
            file = path.trim_end_matches('\t').to_owned();
        } else if let Some(caps) = header.captures(line)
            && !file.is_empty()
        {
            let start: i64 = caps[1].parse()?;
            let count: i64 = caps.get(2).map(|s| s.as_str()).unwrap_or("1").parse()?;
            out.entry(file.clone())
                .or_insert_with(Vec::new)
                .push((start, start + (count - 1).max(0)));
        }
    }
    Ok(out)
}

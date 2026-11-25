use anyhow::Result;
use std::path::Path;

pub(super) fn git(root: &Path, args: &[&str]) -> Result<(bool, Vec<u8>)> {
    git_timed(root, args, "CRG_DISCOVERY_TIMEOUT", 3.)
}

pub(super) use code_review_graph_rs::git::timed as git_timed;
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
    code_review_graph_rs::git::changed_paths(
        root,
        &resolved,
        code_review_graph_rs::git::Scope::Review,
    )
}

pub(super) fn paths(root: &Path, args: &serde_json::Value) -> Result<Vec<String>> {
    if let Some(value) = args.get("changed_files").filter(|v| !v.is_null()) {
        return Ok(serde_json::from_value(value.clone())?);
    }
    discover(root, args["base"].as_str().unwrap_or("HEAD~1"))
}

mod diff;
pub(super) use diff::{Diff, read_diff};

pub(super) fn hunks(
    root: &Path,
    base: &str,
) -> Result<std::collections::HashMap<String, Vec<(i64, i64)>>> {
    Ok(read_diff(root, base)?
        .into_iter()
        .map(|f| (f.path, f.hunks.iter().map(|h| h.range()).collect()))
        .collect())
}

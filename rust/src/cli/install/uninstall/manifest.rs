use super::report::{self, Report};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Serialize)]
struct Manifest {
    version: u8,
    files: Vec<String>,
}
fn allowed(path: &str) -> bool {
    let p = Path::new(path);
    report::clean_relative(p) && !p.starts_with(".git") && !p.starts_with(".code-review-graph")
}
pub(super) fn repo_files(root: &Path) -> Vec<String> {
    let m = root.join(".code-review-graph/install-manifest.json");
    if m.is_symlink() || root.join(".code-review-graph").is_symlink() {
        return vec![];
    }
    let Some(doc) = fs::read_to_string(m)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    else {
        return vec![];
    };
    if doc["version"] != 1 {
        return vec![];
    }
    doc["files"]
        .as_array()
        .and_then(|a| {
            a.iter()
                .map(|v| v.as_str().filter(|p| allowed(p)).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}
pub(super) fn untracked(root: &Path, files: &[String]) -> BTreeSet<PathBuf> {
    if files.is_empty() {
        return BTreeSet::new();
    }
    let mut args = vec!["--literal-pathspecs", "ls-files", "-z", "--"];
    args.extend(files.iter().map(String::as_str));
    let Ok((true, out)) = super::super::changes::git(root, &args) else {
        return BTreeSet::new();
    };
    let tracked: BTreeSet<_> = out.split(|b| *b == 0).collect();
    files
        .iter()
        .filter(|s| !tracked.contains(s.as_bytes()))
        .map(|s| root.join(s))
        .collect()
}
pub(super) fn forget(r: &Report, root: &Path, files: Vec<String>, only: &BTreeSet<PathBuf>) {
    let path = root.join(".code-review-graph/install-manifest.json");
    if !path.is_symlink()
        && !root.join(".code-review-graph").is_symlink()
        && let Ok(before) = fs::read_to_string(&path)
    {
        let left: Vec<_> = files
            .iter()
            .filter(|s| !r.was_touched(&root.join(s)))
            .cloned()
            .collect();
        if left != files {
            if left.is_empty() {
                let _ = fs::remove_file(&path);
            } else {
                let mut files = left;
                files.sort();
                files.dedup();
                if let Ok(after) = serde_json::to_string_pretty(&Manifest { version: 1, files }) {
                    let _ = report::atomic(&path, &before, &(after + "\n"), false);
                }
            }
        }
    }
    let mut dirs: BTreeSet<_> = BTreeSet::new();
    for file in only {
        for p in file.ancestors().skip(1).take_while(|p| *p != root) {
            dirs.insert(p);
        }
    }
    let mut dirs: Vec<_> = dirs.into_iter().collect();
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for p in dirs {
        report::prune(p, root)
    }
}
pub(super) fn codex_skill(r: &mut Report, base: &Path) {
    let dest = base.join("skills/code-review-graph");
    let manifest = dest.join(".code-review-graph-managed.json");
    if ![&dest, &base.join("skills"), &manifest]
        .iter()
        .any(|p| p.is_symlink())
        && let Some(document) = fs::read_to_string(&manifest)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        && let Some(files) = document["files"].as_object()
        && files.iter().all(|(p, v)| {
            report::clean_relative(Path::new(p)) && !p.contains('\\') && v.is_string()
        })
    {
        for (p, h) in files {
            let file = dest.join(p);
            if file
                .ancestors()
                .take_while(|v| *v != base)
                .all(|v| !v.is_symlink())
                && let Ok(bytes) = fs::read(&file)
                && Some(hex::encode(Sha256::digest(bytes)).as_str()) == h.as_str()
            {
                r.skill(&file, base)
            }
        }
        r.skill(&manifest, base);
    }
    if !r.dry() {
        report::prune(&base.join("skills"), base)
    }
}

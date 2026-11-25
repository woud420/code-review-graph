use super::{Plan, changes};
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeSet, path::Path};

pub(super) const START: &str = "# >>> code-review-graph install files >>>";
pub(super) const NOTE: &str =
    "# Added by `code-review-graph install --gitignore`. `uninstall` removes this block.";
pub(super) const END: &str = "# <<< code-review-graph install files <<<";

pub(super) fn valid(entry: &str) -> bool {
    entry.strip_prefix('/').is_some_and(|path| {
        path.trim_end_matches('/').split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
    })
}

fn held(path: &str, tracked: &BTreeSet<&str>) -> bool {
    let prefix = format!("{}/", path.trim_end_matches('/'));
    tracked.contains(path)
        || tracked
            .range(prefix.as_str()..)
            .next()
            .is_some_and(|v| v.starts_with(&prefix))
}

pub(super) fn apply(plan: &mut Plan, root: &Path, dry: bool) -> Result<()> {
    let prefix = if dry { "[dry-run] " } else { "" };
    if !changes::git(root, &["rev-parse", "--is-inside-work-tree"])
        .is_ok_and(|(ok, v)| ok && v.starts_with(b"true"))
    {
        println!(
            "{prefix}Skipped --gitignore: {} is not a Git work tree.",
            root.display()
        );
        return Ok(());
    }
    let (ok, files) = changes::git(root, &["ls-files", "-z"])?;
    ensure!(ok, "git could not list tracked files");
    let files = String::from_utf8_lossy(&files);
    let tracked = files.split('\0').collect::<BTreeSet<_>>();
    let mut entries = BTreeSet::new();
    let mut planned = BTreeSet::new();
    for change in &plan.0 {
        let Ok(rel) = change.path.strip_prefix(root) else {
            continue;
        };
        if change.before.is_some()
            || change.after.is_none()
            || rel.starts_with(".git")
            || rel.starts_with(".code-review-graph")
            || rel == Path::new(".gitignore")
        {
            continue;
        }
        let mut selected = rel;
        for ancestor in rel.ancestors().filter(|p| !p.as_os_str().is_empty()) {
            planned.insert(ancestor.to_owned());
            if root.join(ancestor).exists() || held(&ancestor.to_string_lossy(), &tracked) {
                break;
            }
            selected = ancestor;
        }
        let entry = format!(
            "{}{}",
            selected.display(),
            if selected == rel { "" } else { "/" }
        );
        if !held(&entry, &tracked)
            && valid(&format!("/{entry}"))
            && !changes::git(root, &["check-ignore", "-q", "--", &entry])?.0
        {
            entries.insert(entry);
        }
    }
    plan.edit(root, ".gitignore", ".gitignore".into(), false, |text| {
        let lines = text.split_inclusive('\n').collect::<Vec<_>>();
        let indexes = |marker: &str| lines.iter().enumerate().filter_map(|(i,l)| (l.trim_end_matches(['\r','\n']) == marker).then_some(i)).collect::<Vec<_>>();
        let start = indexes(START);
        let end = indexes(END);
        let range = match (start.as_slice(), end.as_slice()) {
            ([], []) => None,
            ([lo], [hi]) if lo < hi && lines[lo+1..*hi].iter().all(|l| {
                let line = l.trim_end_matches(['\r','\n']); line == NOTE || valid(line)
            }) => Some((*lo,*hi)),
            _ => {
                println!("{prefix}Left the code-review-graph install files block in .gitignore alone: it was edited by hand.");
                return Ok(text.into());
            }
        };
        let mut old = BTreeSet::new();
        if let Some((lo,hi)) = range {
            for line in &lines[lo+1..hi] {
                let Some(entry) = line.trim_end_matches(['\r','\n']).strip_prefix('/') else { continue };
                let path = Path::new(entry.trim_end_matches('/'));
                if (root.join(path).symlink_metadata().is_ok() || planned.contains(path)) && !held(entry,&tracked) {
                    old.insert(entry.to_owned());
                }
            }
        }
        let added: Vec<_> = entries.difference(&old).cloned().collect();
        entries.extend(old);
        let nl = if lines.first().is_some_and(|l| l.ends_with("\r\n")) { "\r\n" } else { "\n" };
        let block = if entries.is_empty() { String::new() } else {
            let mut lines = vec![START.to_owned(),NOTE.to_owned()];
            lines.extend(entries.iter().map(|e| format!("/{e}")));
            lines.push(END.to_owned());
            lines.join(nl) + nl
        };
        let next = if let Some((lo,hi)) = range {
            let mut before = lines[..lo].concat();
            if hi+1 == lines.len() && !text.ends_with('\n') {
                if block.is_empty() && before.ends_with(nl) { before.truncate(before.len()-nl.len()); }
                before + block.trim_end_matches(['\r','\n'])
            } else { before + &block + &lines[hi+1..].concat() }
        } else if block.is_empty() || text.is_empty() || text.ends_with('\n') {
            text.to_owned()+&block
        } else { format!("{text}{nl}{}",block.strip_suffix(nl).context("invalid gitignore block")?) };
        if !added.is_empty() {
            let verb = if dry { "Would add" } else { "Added" };
            println!("{prefix}{verb} to .gitignore, in the code-review-graph install files block:");
            for entry in added { println!("    /{entry}"); }
        } else if text != next {
            println!("{prefix}{} entries for paths that are gone from the .gitignore block.", if dry { "Would remove" } else { "Removed" });
        } else if entries.is_empty() {
            println!("{prefix}No files to add to .gitignore: install created none that git shows.");
        } else { println!("{prefix}The .gitignore block already lists every file install created."); }
        Ok(next)
    })
}

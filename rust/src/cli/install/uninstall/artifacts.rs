use super::{catalog, report::Report};
use std::path::Path;

pub(super) fn instructions(r: &mut Report, root: &Path) {
    for file in catalog("instruction_paths") {
        let p = root.join(file);
        let Some(raw) = r.read(&p, root) else {
            continue;
        };
        let mut next = raw.clone();
        for known in catalog("instructions") {
            while let Some((a, b)) = next.split_once(known) {
                next = if b.trim().is_empty() {
                    if a.trim().is_empty() {
                        String::new()
                    } else {
                        format!("{}\n", a.trim_end())
                    }
                } else if a.trim().is_empty() {
                    b.trim_start_matches('\n').to_owned()
                } else {
                    format!(
                        "{}\n\n{}",
                        a.trim_end_matches('\n'),
                        b.trim_start_matches('\n')
                    )
                };
            }
        }
        if next.contains("<!-- code-review-graph MCP tools -->") {
            r.skip(&p,if next==raw{"marked instruction section differs from a known installed section; left unchanged"}else{"a further marked instruction section differs from a known installed section; left unchanged"})
        }
        if next != raw {
            if next.is_empty() {
                r.delete(&p, root, "remove generated instruction file", false)
            } else {
                r.write(
                    &p,
                    root,
                    &raw,
                    &next,
                    "remove code-review-graph instruction section",
                )
            }
        }
    }
}
pub(super) fn git_hooks(r: &mut Report, root: &Path) {
    for name in ["pre-commit", "post-checkout", "post-merge", "post-rewrite"] {
        let relative = format!("hooks/{name}");
        let path = super::super::changes::git(root, &["rev-parse", "--git-path", &relative])
            .ok()
            .filter(|(ok, _)| *ok)
            .and_then(|(_, s)| String::from_utf8(s).ok())
            .map(|s| root.join(s.trim()))
            .unwrap_or_else(|| root.join(".git").join(relative));
        let Some(raw) = r.read(&path, root) else {
            continue;
        };
        let label = if name == "pre-commit" {
            "pre-commit hook"
        } else {
            "sync hook"
        };
        let start = format!("# >>> code-review-graph {label} >>>");
        let end = format!("# <<< code-review-graph {label} <<<");
        let mut next = raw.clone();
        let mut balanced = true;
        while let Some(i) = next.find(&start) {
            let Some(j) = next[i + start.len()..].find(&end) else {
                balanced = false;
                break;
            };
            let j = i + start.len() + j;
            if next[i + start.len()..j].contains(&start) {
                balanced = false;
                break;
            }
            let right = j + end.len();
            let right = right + usize::from(next.as_bytes().get(right) == Some(&b'\n'));
            next.replace_range(i..right, "");
        }
        if !balanced {
            r.skip(
                &path,
                "a code-review-graph begin marker has no matching end marker; left unchanged",
            );
            continue;
        }
        if name == "pre-commit" {
            for legacy in catalog("legacy_hooks") {
                next = next.replace(legacy, "")
            }
        }
        if next == raw {
            continue;
        }
        if next
            .lines()
            .any(|line| !line.trim().is_empty() && !line.trim().starts_with("#!"))
        {
            r.write(
                &path,
                root,
                &raw,
                &format!("{}\n", next.trim_end_matches('\n')),
                "remove code-review-graph hook block",
            );
        } else {
            r.delete(
                &path,
                root,
                "remove hook containing only code-review-graph",
                false,
            )
        }
    }
}
pub(super) fn ignore(r: &mut Report, root: &Path) {
    let p = root.join(".gitignore");
    let Some(raw) = r.read(&p, root) else { return };
    let mut lines: Vec<_> = raw.split_inclusive('\n').collect();
    let starts = lines
        .iter()
        .enumerate()
        .filter(|(_, s)| s.trim_end_matches(['\r', '\n']) == super::super::ignore_block::START)
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    let ends = lines
        .iter()
        .enumerate()
        .filter(|(_, s)| s.trim_end_matches(['\r', '\n']) == super::super::ignore_block::END)
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    if let ([a], [b]) = (starts.as_slice(), ends.as_slice())
        && a <= b
        && lines[a + 1..*b].iter().all(|s| {
            let s = s.trim_end_matches(['\r', '\n']);
            s == super::super::ignore_block::NOTE || super::super::ignore_block::valid(s)
        })
    {
        if *a > 0 && *b + 1 == lines.len() && !lines[*b].ends_with('\n') {
            lines[*a - 1] = lines[*a - 1].trim_end_matches(['\r', '\n'])
        }
        lines.drain(*a..=*b);
    } else if !starts.is_empty() || !ends.is_empty() {
        r.skip(
            &p,
            "code-review-graph install files block was edited by hand; left unchanged",
        )
    }
    let mut out = String::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == "# Added by code-review-graph"
            && lines
                .get(i + 1)
                .is_some_and(|v| matches!(v.trim(), ".code-review-graph" | ".code-review-graph/"))
        {
            i += 2;
            continue;
        }
        out.push_str(lines[i]);
        i += 1;
    }
    if raw == out {
        return;
    }
    if out.trim().is_empty() {
        r.delete(&p, root, "remove generated .gitignore", false)
    } else {
        r.write(&p, root, &raw, &out, "remove installer-owned ignore block")
    }
}

use super::*;
use std::{collections::HashSet, path::Component};

type Region = (usize, usize);
struct Source {
    lines: Vec<String>,
    regions: Vec<Region>,
    granted: Vec<Region>,
    used: usize,
}

fn regions(count: usize, hunks: &[(i64, i64)], nodes: &[&Value], cap: usize) -> Vec<Region> {
    if count == 0 {
        return vec![];
    }
    if count <= cap {
        return vec![(0, count)];
    }
    let definitions: Vec<_> = nodes
        .iter()
        .filter_map(|n| {
            let start = n["line_start"].as_i64()?;
            let end = n["line_end"].as_i64()?;
            (start > 0 && end >= start).then_some((start, end))
        })
        .collect();
    let spans = if hunks.is_empty() {
        definitions
    } else {
        hunks
            .iter()
            .map(|&(a, b)| {
                let enclosing = definitions
                    .iter()
                    .filter(|&&(s, e)| s <= b && e >= a && e - s < 40)
                    .min_by_key(|&&(s, e)| (e - s, s, e));
                enclosing.map_or((a - 4, b + 4), |&(s, e)| (s.min(a), e.max(b)))
            })
            .collect()
    };
    if spans.is_empty() {
        return vec![(0, count.min(50))];
    }
    let mut spans: Vec<_> = spans
        .into_iter()
        .map(|(s, e)| ((s - 3).max(0) as usize, count.min((e + 2).max(0) as usize)))
        .filter(|(s, e)| s < e)
        .collect();
    spans.sort_unstable();
    let mut merged: Vec<Region> = Vec::new();
    for (s, e) in spans {
        if let Some((start, end)) = merged.last_mut()
            && s <= *end + 1
            && (*end).max(e) - *start <= cap
        {
            *end = (*end).max(e);
        } else {
            merged.push((s, e));
        }
    }
    merged
}

impl Source {
    fn render(&self) -> String {
        let mut previous = None;
        let mut parts = Vec::new();
        for &(start, end) in &self.granted {
            if previous.is_some_and(|p| start > p) {
                parts.push("...".to_owned())
            }
            for (offset, line) in self.lines[start..end].iter().enumerate() {
                parts.push(format!("{}: {line}", start + offset + 1));
            }
            previous = Some(end);
        }
        let omitted = self.regions.len() - self.granted.len();
        if omitted > 0 {
            parts.push(format!("... ({omitted} more changed region(s) not shown)"))
        }
        parts.join("\n")
    }
}

fn allowed(path: &Path, root: &Path, followed: &HashSet<String>) -> bool {
    if path
        .canonicalize()
        .ok()
        .is_some_and(|p| p.starts_with(root))
    {
        return true;
    }
    let Some(rel) = path.strip_prefix(root).ok() else {
        return false;
    };
    rel.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        && followed.contains(path.to_string_lossy().as_ref())
}

pub(super) fn add(
    conn: &Connection,
    context: &mut Value,
    root: &Path,
    base: &str,
    files: &[String],
    grouped: &HashMap<String, Vec<&Value>>,
    per_file: usize,
) -> Result<()> {
    let hunks = crate::cli::changes::hunks(root, base)?;
    let followed: HashSet<String> = conn
        .query_row(
            "SELECT value FROM metadata WHERE key='symlink_followed_files'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default();
    let share = per_file.min(320);
    let cap = share.min(120);
    let mut sources = HashMap::new();
    let mut snippets = json!({});
    for file in files {
        let path = root.join(file);
        if !path.is_file() {
            continue;
        }
        if !allowed(&path, root, &followed) {
            snippets[file] = json!("(outside the repository)");
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            snippets[file] = json!("(could not read file)");
            continue;
        };
        let lines = String::from_utf8_lossy(&bytes)
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let regions = regions(
            lines.len(),
            hunks.get(file).map(Vec::as_slice).unwrap_or_default(),
            &grouped[file],
            cap,
        );
        sources.insert(
            file.as_str(),
            Source {
                lines,
                regions,
                granted: vec![],
                used: 0,
            },
        );
    }
    let depth = sources
        .values()
        .map(|s| s.regions.len())
        .max()
        .unwrap_or_default();
    let mut remaining = 800;
    for i in 0..depth {
        for file in files {
            let Some(source) = sources.get_mut(file.as_str()) else {
                continue;
            };
            let Some(&(a, b)) = source.regions.get(i) else {
                continue;
            };
            let cost = (b - a).min(cap);
            if cost > 0 && cost <= remaining && cost <= share - source.used {
                source.used += cost;
                remaining -= cost;
                source.granted.push((a, a + cost));
            }
        }
    }
    let mut shown = 0;
    let mut total = 0;
    let mut clipped = false;
    let mut incomplete = json!({});
    for file in files {
        let Some(source) = sources.get_mut(file.as_str()) else {
            continue;
        };
        let have = source.granted.len();
        let want = source.regions.len();
        shown += have;
        total += want;
        if have < want {
            incomplete[file] = json!([have, want]);
        }
        clipped |= source.granted.iter().any(|r| !source.regions.contains(r));
        if have > 0 {
            source.granted.sort_unstable();
            snippets[file] = json!(source.render());
        }
    }
    context["source_snippets"] = snippets;
    context["source_regions"] = json!({"shown":shown,"total":total,"incomplete":incomplete});
    if shown < total || clipped {
        context["source_truncated"] = json!(true);
        context["truncated"] = json!(true);
    }
    Ok(())
}

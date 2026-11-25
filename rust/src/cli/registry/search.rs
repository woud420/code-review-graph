use crate::cli::{query, search, store};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::HashSet, path::Path};

pub(crate) fn run(args: &Value) -> Result<Value> {
    let opts: search::Options = serde_json::from_value(args.clone())?;
    ensure!(
        opts.limit > 0,
        "limit must be an integer greater than or equal to 1"
    );
    let cap = args["max_results"].as_i64().unwrap_or(50);
    ensure!(
        cap > 0,
        "max_results must be an integer greater than or equal to 1"
    );
    let cap = cap.min(100) as usize;
    let repos = super::list()?;
    if repos.is_empty() {
        return Ok(
            json!({"status":"ok","summary":"No repositories registered. Use 'register' to add repos.","results":[]}),
        );
    }
    let requested = args["repos"].as_array().filter(|xs| !xs.is_empty());
    let mut unknown = Vec::new();
    let mut ambiguous = Vec::new();
    let mut wanted = HashSet::new();
    if let Some(requested) = requested {
        for raw in requested {
            let name = raw.as_str().context("repository name must be a string")?;
            let mut matches: Vec<_> = repos
                .iter()
                .enumerate()
                .filter(|(_, r)| r["alias"].as_str() == Some(name))
                .map(|(id, _)| id)
                .collect();
            if matches.is_empty() {
                matches = repos
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| {
                        r["path"]
                            .as_str()
                            .and_then(|p| Path::new(p).file_name()?.to_str())
                            == Some(name)
                    })
                    .map(|(i, _)| i)
                    .collect();
            }
            if matches.is_empty() {
                unknown.push(query::clean(name));
            } else if matches.len() > 1 {
                ambiguous.push(query::clean(name));
            }
            wanted.extend(matches);
        }
    } else {
        wanted.extend(0..repos.len());
    }
    let mut rows = Vec::new();
    let mut searched = Vec::new();
    for (id, entry) in repos.iter().enumerate() {
        if !wanted.contains(&id) {
            continue;
        }
        let path = Path::new(
            entry["path"]
                .as_str()
                .context("registered repository has no path")?,
        );
        let database = store::database(path, None);
        if !database.is_file() {
            continue;
        }
        let conn = match store::open(&database) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("WARNING: Search failed for {}: {e}", path.display());
                continue;
            }
        };
        let hits = match search::ranked(&conn, path, &opts, opts.limit) {
            Ok((_, hits)) => hits,
            Err(e) => {
                eprintln!("WARNING: Search failed for {}: {e}", path.display());
                continue;
            }
        };
        let alias = entry["alias"]
            .as_str()
            .or_else(|| path.file_name()?.to_str())
            .unwrap_or_default();
        searched.push(alias);
        for (rank, mut row) in hits.into_iter().enumerate() {
            row["repo"] = json!(alias);
            row["repo_path"] = json!(path);
            rows.push(((rank, id), row));
        }
    }
    rows.sort_by_key(|(rank, _)| *rank);
    let total = rows.len();
    let visible: Vec<_> = rows.into_iter().take(cap).map(|(_, r)| r).collect();
    let mut summary = if wanted.is_empty() {
        format!(
            "No registered repository matches the {} name(s) given. Use 'repos' to list the registry.",
            unknown.len()
        )
    } else {
        format!(
            "Found {total} result(s) across {} repo(s) for '{}'",
            searched.len(),
            opts.query
        )
    };
    if total > cap {
        summary += &format!(", showing {cap} of {total}");
    }
    if !wanted.is_empty() {
        if !unknown.is_empty() {
            summary += &format!("; {} name(s) matched no repository", unknown.len());
        }
        if !ambiguous.is_empty() {
            summary += &format!("; {} name(s) matched several", ambiguous.len());
        }
    }
    let mut out = json!({"status":"ok","summary":summary,"results":visible,"total":total,"truncated":total>cap,"repos_searched":searched});
    if requested.is_some() {
        for (key, mut names) in [("unknown", unknown), ("ambiguous", ambiguous)] {
            let total = names.len();
            names.truncate(20);
            out[key] = json!(names);
            out[format!("{key}_total")] = json!(total);
            out[format!("{key}_truncated")] = json!(total > 20);
        }
    }
    Ok(out)
}

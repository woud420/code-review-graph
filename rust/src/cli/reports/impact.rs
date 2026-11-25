pub(super) mod traversal;
use super::*;
use std::collections::{HashMap, HashSet};
use traversal::Radius;

pub(super) fn savings(
    out: &mut Value,
    conn: &Connection,
    root: &Path,
    paths: &[String],
    tool: &str,
    detail: &str,
    depth: i64,
) {
    let baseline: usize = paths
        .iter()
        .filter_map(|p| root.join(p).metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| (m.len() as usize).div_ceil(4).max(1))
        .sum();
    if baseline > 0 {
        let returned = super::architecture::tokens(out);
        super::super::history::record(
            conn,
            tool,
            detail,
            Some((paths.len(), depth)),
            baseline,
            returned,
        );
        let saved = baseline.saturating_sub(returned);
        out["context_savings"] = json!({"estimated":true,"saved_tokens":saved,"saved_percent":(100.0*saved as f64/baseline as f64).round_ties_even() as usize});
    }
}

fn compact(paths: &[String], r: &Radius, root: &Path, cap: usize, resolution: &str) -> Value {
    let rel = |s: &String| {
        Path::new(s)
            .strip_prefix(root)
            .unwrap_or_else(|_| Path::new(s))
            .to_string_lossy()
            .into_owned()
    };
    let visible = r.files.len().min(cap);
    json!({"status":"ok","summary":format!("Showing {visible} of {} impacted files (best-first).",r.files.len()),"changed_files":paths.iter().take(cap).map(rel).collect::<Vec<_>>(),"changed_file_count":paths.len(),"changed_files_omitted":paths.len().saturating_sub(cap),"unmatched_files":r.unmatched.iter().take(cap).map(rel).collect::<Vec<_>>(),"unmatched_files_total":r.unmatched.len(),"unmatched_files_truncated":r.unmatched.len()>cap,"impacted_files":r.files.iter().take(cap).map(rel).collect::<Vec<_>>(),"impacted_file_count":r.files.len(),"impacted_files_omitted":r.files.len()-visible,"total_impacted":r.total,"truncated":r.impacted.len()<r.total||paths.len()>cap||r.files.len()>cap||r.unmatched.len()>cap,"resolution":resolution,"unresolved_call_sites":r.unresolved})
}
fn confidence(conn: &Connection, root: &Path, paths: &[String], r: &Radius) -> Result<String> {
    use super::super::query::empty_confidence;
    if let Some(file) = r.resolved.first() {
        let node = r.changed.iter().find(|n| text(n, "file_path") == file);
        let msg = empty_confidence(conn, root, "", node, "impact_radius")?;
        if msg.contains("is indexed and the graph is current") {
            return Ok(
                "changed files are indexed and the graph is current, so this 0 is a real absence"
                    .into(),
            );
        }
        if msg.contains("is indexed and no such edge is recorded") {
            return Ok(
                "changed files are indexed and nothing depends on them; graph currency unverified"
                    .into(),
            );
        }
        return Ok(msg);
    }
    let name = paths
        .first()
        .and_then(|s| Path::new(s).file_name()?.to_str())
        .unwrap_or_default();
    let prefix = "target not indexed: no node matching '";
    let tail = "', so this 0 is not evidence that none exist";
    let cap = 140 - prefix.len() - tail.len();
    let short = if name.chars().count() <= cap {
        name.to_owned()
    } else {
        format!("{}~", name.chars().take(cap - 1).collect::<String>())
    };
    Ok(format!("{prefix}{short}{tail}"))
}

pub(super) fn run(conn: &Connection, root: &Path, args: &Value) -> Result<Value> {
    let resolution = args["resolution"].as_str().unwrap_or("all");
    ensure!(
        matches!(resolution, "all" | "direct"),
        "resolution must be one of ['all', 'direct'], got '{resolution}'"
    );
    let cap = positive(args, "max_results", 500, i64::MAX as usize)?;
    let file_cap = positive(args, "max_files", 25, 200)?;
    let depth = integer(args, "max_depth", 2)?;
    let detail = args["detail_level"].as_str().unwrap_or("standard");
    let paths = match super::super::changes::paths(root, args) {
        Ok(p) => p,
        Err(e) => {
            return Ok(json!({"status":"error","error":e.to_string(),"summary":e.to_string()}));
        }
    };
    if paths.is_empty() && detail != "compact" {
        return Ok(
            json!({"status":"ok","summary":"No changed files detected.","changed_nodes":[],"impacted_nodes":[],"impacted_files":[],"truncated":false,"total_impacted":0}),
        );
    }
    let mut r = traversal::run(
        conn,
        root,
        &paths,
        depth.max(0) as usize,
        cap,
        resolution == "direct",
    )?;
    if detail == "compact" {
        let mut out = compact(&paths, &r, root, file_cap, resolution);
        if !paths.is_empty() && r.impacted.is_empty() {
            out["confidence"] = json!(confidence(conn, root, &paths, &r)?);
        }
        savings(
            &mut out,
            conn,
            root,
            &paths,
            "get_impact_radius_tool",
            detail,
            depth,
        );
        return Ok(out);
    }
    let changed_count = r.changed.len();
    let seeds: HashSet<_> = r
        .changed
        .iter()
        .map(|n| text(n, "qualified_name"))
        .collect();
    let node_cap = cap.min(100);
    let edge_cap = cap.min(150);
    let file_limit = cap.min(200);
    let unmatched_limit = if detail == "minimal" { 5 } else { file_limit };
    let mut sites: HashMap<&str, Vec<&Value>> = HashMap::new();
    for e in &r.edges {
        if matches!(text(e, "kind"), "CALLS" | "REFERENCES")
            && seeds.contains(text(e, "target"))
            && !seeds.contains(text(e, "source"))
        {
            sites.entry(text(e, "source")).or_default().push(e);
        }
    }
    for n in r.impacted.iter_mut().take(node_cap) {
        if let Some(list) = sites.get(text(n, "qualified_name"))
            && let Some(edge) = list
                .iter()
                .min_by_key(|e| (text(e, "file_path"), e["line"].as_i64()))
        {
            let mut site = json!({"line":edge["line"]});
            if n["file_path"] != edge["file_path"] {
                site["file"] = edge["file_path"].clone();
            }
            n["call_site"] = site;
            if list.len() > 1 {
                n["call_site_count"] = json!(list.len());
            }
        }
    }
    r.edges
        .sort_by_key(|e| !seeds.contains(text(e, "source")) && !seeds.contains(text(e, "target")));
    let node_omitted = r.total.saturating_sub(node_cap);
    let changed_omitted = changed_count.saturating_sub(node_cap);
    let edge_omitted = r.edges.len().saturating_sub(edge_cap);
    let file_omitted = r.files.len().saturating_sub(file_limit);
    let unmatched_cut = r.unmatched.len() > unmatched_limit;
    let truncated =
        node_omitted + changed_omitted + edge_omitted + file_omitted > 0 || unmatched_cut;
    let risk = if r.resolved.is_empty() {
        "unknown"
    } else if r.total > 20 {
        "high"
    } else if r.total > 5 {
        "medium"
    } else {
        "low"
    };
    let mut summary = format!(
        "Blast radius for {} changed file(s):\n  - {changed_count} nodes directly changed\n  - {} nodes impacted (within {depth} hops)\n  - {} additional files affected",
        paths.len(),
        r.total,
        r.files.len()
    );
    if !r.unmatched.is_empty() {
        let mut joined = r
            .unmatched
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if r.unmatched.len() > 5 {
            joined += &format!(" (+{} more)", r.unmatched.len() - 5);
        }
        summary += &format!(
            "\n  - {} requested file(s) are not indexed: {joined}",
            r.unmatched.len()
        );
    }
    if node_omitted > 0 {
        summary += &format!(
            "\n  - Results truncated: showing {} of {} impacted nodes",
            r.total - node_omitted,
            r.total
        );
    }
    if changed_omitted + edge_omitted + file_omitted > 0 {
        summary += &format!(
            "\n  - Also truncated: {edge_omitted} edges, {changed_omitted} changed nodes, {file_omitted} impacted files omitted"
        );
    }
    let unmatched = r.unmatched.iter().take(unmatched_limit).collect::<Vec<_>>();
    let mut out = if detail == "minimal" {
        json!({"status":"ok","summary":summary,"risk":risk,"unmatched_files":unmatched,"unmatched_files_total":r.unmatched.len(),"unmatched_files_truncated":unmatched_cut,"impacted_file_count":r.files.len(),"key_entities":r.impacted.iter().take(node_cap.min(5)).map(|n|text(n,"name")).collect::<Vec<_>>(),"truncated":truncated,"nodes_omitted":node_omitted,"unresolved_call_sites":r.unresolved})
    } else {
        json!({"status":"ok","risk":risk,"unmatched_files":unmatched,"unmatched_files_total":r.unmatched.len(),"unmatched_files_truncated":unmatched_cut,"summary":summary,"changed_files":paths,"changed_nodes":&r.changed[..changed_count.min(node_cap)],"changed_nodes_omitted":changed_omitted,"impacted_nodes":&r.impacted[..r.impacted.len().min(node_cap)],"impacted_files":&r.files[..r.files.len().min(file_limit)],"impacted_files_omitted":file_omitted,"edges":&r.edges[..r.edges.len().min(edge_cap)],"edges_omitted":edge_omitted,"truncated":truncated,"total_impacted":r.total,"nodes_omitted":node_omitted,"resolution":resolution,"unresolved_call_sites":r.unresolved})
    };
    if r.impacted.is_empty() {
        out["confidence"] = json!(confidence(conn, root, &paths, &r)?);
    }
    savings(
        &mut out,
        conn,
        root,
        &paths,
        "get_impact_radius_tool",
        detail,
        depth,
    );
    Ok(out)
}

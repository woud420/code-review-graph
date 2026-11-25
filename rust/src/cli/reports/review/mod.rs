pub(super) mod risk;
mod source;

use super::{
    impact::{
        self,
        traversal::{self, Radius},
    },
    *,
};
use crate::cli::changes;
use std::collections::HashMap;

pub(super) fn run(conn: &Connection, root: &Path, args: &Value) -> Result<Value> {
    let limit = positive(args, "max_results", 50, usize::MAX)?;
    let file_cap = positive(args, "max_files", 25, 200)?;
    let line_cap = positive(args, "max_lines_per_file", 200, 500)?;
    let detail = args["detail_level"].as_str().unwrap_or("standard");
    let depth = integer(args, "max_depth", 2)?;
    let base = args["base"].as_str().unwrap_or("HEAD~1");
    let files = match changes::paths(root, args) {
        Ok(f) => f,
        Err(e) => {
            return Ok(json!({"status":"error","error":e.to_string(),"summary":e.to_string()}));
        }
    };
    let base = match changes::resolve_base(root, base) {
        Ok(b) => b,
        Err(e) => {
            return Ok(json!({"status":"error","error":e.to_string(),"summary":e.to_string()}));
        }
    };
    if files.is_empty() {
        return Ok(
            json!({"status":"ok","summary":"No changes detected. Nothing to review.","context":{}}),
        );
    }
    let cap = std::env::var("CRG_MAX_IMPACT_NODES")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(500);
    let mut radius = traversal::run(conn, root, &files, depth.max(0) as usize, cap, false)?;
    for node in &mut radius.impacted {
        if let Some(obj) = node.as_object_mut() {
            obj.remove("impact_score");
        }
    }
    let risk = if radius.resolved.is_empty() {
        "unknown"
    } else if radius.total > 20 {
        "high"
    } else if radius.total > 5 {
        "medium"
    } else {
        "low"
    };
    let gaps = risk::gaps(&radius, root);
    let unmatched = radius.unmatched.len();
    let unmatched_limit = if detail == "minimal" { 5 } else { file_cap };
    let warning = if unmatched > 0 {
        let tail = if unmatched > 5 {
            format!(" (+{} more)", unmatched - 5)
        } else {
            String::new()
        };
        format!(
            "\n  - {unmatched} requested file(s) are not indexed: {}{tail}",
            radius.unmatched[..unmatched.min(5)].join(", ")
        )
    } else {
        String::new()
    };
    let mut out = if detail == "minimal" {
        let summary = format!(
            "Review context for {} changed file(s):\n  - Risk: {risk}\n  - {} impacted nodes in {} files{warning}",
            files.len(),
            radius.impacted.len(),
            radius.files.len()
        );
        json!({"status":"ok","summary":summary,"risk":risk,"unmatched_files":&radius.unmatched[..unmatched.min(unmatched_limit)],"unmatched_files_total":unmatched,"unmatched_files_truncated":unmatched>unmatched_limit,"changed_file_count":files.len(),"impacted_file_count":radius.files.len(),"key_entities":radius.changed.iter().take(5).map(|n|text(n,"name")).collect::<Vec<_>>(),"test_gaps":gaps.len(),"next_tool_suggestions":["detect_changes_tool","get_affected_flows_tool","get_impact_radius_tool"]})
    } else {
        let grouped: HashMap<_, _> = files
            .iter()
            .map(|f| {
                let full = root.join(f);
                let nodes = radius
                    .changed
                    .iter()
                    .filter(|n| {
                        let path = Path::new(text(n, "file_path"));
                        path == full || path == Path::new(f)
                    })
                    .collect();
                (f.clone(), nodes)
            })
            .collect();
        let scores = risk::by_file(conn, root, &files, &grouped)?;
        let mut ranked = files.clone();
        ranked.sort_by(|a, b| scores[b].total_cmp(&scores[a]));
        ranked.truncate(file_cap);
        let mut file_risk = json!({});
        for file in &ranked {
            file_risk[file] = json!(scores[file]);
        }
        let cn = radius.changed.len();
        let ni = radius.impacted.len();
        let fe = radius.files.len();
        let et = radius.edges.len();
        let nc = limit.min(200);
        let ec = limit.min(300);
        let cut = files.len() > file_cap
            || fe > file_cap
            || cn > nc
            || ni > nc
            || et > ec
            || unmatched > file_cap;
        let mut context = json!({"changed_files":ranked,"changed_files_total":files.len(),"file_risk":file_risk,"impacted_files":&radius.files[..fe.min(file_cap)],"impacted_files_total":fe,"graph":{"changed_nodes":&radius.changed[..cn.min(nc)],"changed_nodes_total":cn,"impacted_nodes":&radius.impacted[..ni.min(nc)],"impacted_nodes_total":ni,"edges":&radius.edges[..et.min(ec)],"edges_total":et},"truncated":cut});
        if args["include_source"].as_bool().unwrap_or(true) {
            source::add(conn, &mut context, root, &base, &ranked, &grouped, line_cap)?;
        }
        let guidance = risk::guidance(conn, &radius, root, &gaps)?;
        context["review_guidance"] = json!(guidance);
        let summary = format!(
            "Review context for {} changed file(s){}:\n  - {cn} directly changed nodes{}\n  - {ni} impacted nodes in {fe} files{}\n\nReview guidance:\n{guidance}{warning}",
            files.len(),
            shown(ranked.len(), files.len()),
            shown(cn.min(nc), cn),
            shown(ni.min(nc), ni)
        );
        json!({"status":"ok","risk":risk,"unmatched_files":&radius.unmatched[..unmatched.min(unmatched_limit)],"unmatched_files_total":unmatched,"unmatched_files_truncated":unmatched>unmatched_limit,"summary":summary,"context":context})
    };
    impact::savings(
        &mut out,
        conn,
        root,
        &files,
        "get_review_context_tool",
        detail,
        depth,
    );
    Ok(out)
}

use super::{store, wire};
use anyhow::{Result, ensure};
use code_review_graph_rs::canonical::{self, postprocess::Level};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::Path;

fn full_resolvers(report: &mut Value, db: &Connection) -> Result<()> {
    let files = |language: &str| {
        db.query_row(
            "SELECT count(*) FROM nodes WHERE kind='File' AND language=?",
            [language],
            |r| r.get::<_, i64>(0),
        )
    };
    let count = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0));
    let imports = |result: &str| {
        db.query_row("SELECT count(*) FROM edges WHERE kind='IMPORTS_FROM' AND json_extract(extra,'$.import_resolution')=?", [result],|r|r.get::<_,i64>(0))
    };
    let resolved = imports("repository_suffix")?;
    let ambiguous = imports("ambiguous")?;
    report["python_resolution"] = json!({"files_indexed":files("python")?,"imports_updated":resolved+ambiguous+imports("unresolved")?,"imports_resolved":resolved,"imports_ambiguous":ambiguous});
    report["rescript_resolution"] =
        json!({"files_indexed":files("rescript")?,"calls_resolved":0,"imports_resolved":0});
    report["spring_resolution"] = json!({"files_indexed":files("java")?,"calls_resolved":0});
    report["aop_resolution"] =
        json!({"aspects_indexed":0,"advice_resolved":0,"calls_created":0,"stale_calls_removed":0});
    report["event_resolution"] =
        json!({"events_indexed":0,"calls_emitted":0,"stale_calls_removed":0});
    report["temporal_resolution"] = json!({"files_indexed":files("java")?,"calls_resolved":0});
    report["hcl_resolution"] =
        json!({"files_indexed":files("hcl")?,"references_resolved":0,"imports_resolved":0});
    report["scoped_resolution"] = json!({"files_indexed":files("rust")?+files("php")?+files("csharp")?,"calls_resolved":count("SELECT count(*) FROM edges WHERE kind='CALLS' AND json_type(extra,'$.scoped_resolved') IS NOT NULL")?});
    report["jedi_enrichment"] = json!({"skipped":true,"reason":"not requested"});
    Ok(())
}

pub fn run(root: &Path, args: &Value) -> Result<Value> {
    ensure!(
        store::project_root(root).is_some(),
        "repo_root does not look like a project root (no .git, .svn, or .code-review-graph directory found): {}",
        root.display()
    );
    ensure!(
        args["base"].is_null(),
        "explicit incremental base is not supported by the native writer"
    );
    ensure!(
        args["recurse_submodules"] != true || !root.join(".gitmodules").exists(),
        "recursive submodule indexing is not supported by the native writer"
    );
    let level = match args["postprocess"].as_str().unwrap_or("full") {
        "full" => Level::Full,
        "minimal" => Level::Minimal,
        "none" => Level::None,
        other => anyhow::bail!("invalid postprocess level: {other}"),
    };
    let db_path = store::database(root, None);
    let result = match canonical::index(
        root,
        &db_path,
        args["full_rebuild"].as_bool().unwrap_or(false),
        level,
        false,
    ) {
        Err(e) if e.to_string() == "another writer is using this graph" => {
            let lock = db_path.with_file_name("write.lock");
            let pid = std::fs::read_to_string(&lock)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok());
            let holder = pid.map_or_else(|| "another process".into(), |n| format!("PID {n}"));
            return Ok(
                json!({"status":"contended","skipped":true,"build_type":"none","holder_pid":pid,"lock_path":lock,
                "files_parsed":0,"files_updated":0,"total_nodes":0,"total_edges":0,"errors":[],
                "summary":format!("Another code-review-graph write is already running for this repository ({holder}); skipped instead of repeating the work. Pass wait_for_lock=True (CLI: --wait) to block until it finishes.")}),
            );
        }
        other => other?,
    };
    let db = store::open(&db_path)?;
    let no_change = !result.full && result.files_parsed == 0;
    let mut report = if result.full {
        json!({"files_parsed":result.files_parsed,"stale_files_removed":result.files_deleted,"total_nodes":result.nodes,"total_edges":result.edges,"errors":[]})
    } else {
        let mut value = json!({"files_updated":result.files_parsed,"total_nodes":if no_change {0} else {result.nodes},
            "total_edges":if no_change {0} else {result.edges},"changed_files":result.changed_files,"dependent_files":result.dependent_files,"stale_files_removed":result.files_deleted});
        if no_change {
            value["flow_entry_point_qns"] = json!([]);
        }
        value["communities_were_affected"] = false.into();
        value["errors"] = json!([]);
        value["freshness_advanced"] = result.base.is_some().into();
        value
    };
    if result.full {
        full_resolvers(&mut report, &db)?;
    } else {
        for name in [
            "python_resolution",
            "rescript_resolution",
            "spring_resolution",
            "aop_resolution",
            "event_resolution",
            "temporal_resolution",
            "hcl_resolution",
            "scoped_resolution",
            "jedi_enrichment",
        ] {
            report[name] = Value::Null;
        }
    }
    report["status"] = "ok".into();
    report["build_type"] = if result.full { "full" } else { "incremental" }.into();
    report["base_resolved"] = json!(result.base);
    report["summary"]=if result.full {
        format!("Full build complete: parsed {} files, created {} nodes and {} edges.",result.files_parsed,result.nodes,result.edges)
    } else if no_change && result.base.is_some() {"No changes detected. Graph is up to date.".into()}
    else if no_change {"No graph changes detected. Freshness metadata was not advanced.".into()}
    else {format!("Incremental update: {} files re-parsed, {} nodes and {} edges updated. Changed: {}. Dependents also updated: {}.",
        result.files_parsed,result.nodes,result.edges,wire::value_repr(&report["changed_files"]),wire::value_repr(&report["dependent_files"]))}.into();
    report["postprocess_level"] = match result.effective_level {
        Level::None => "none",
        Level::Minimal => "minimal",
        Level::Full => "full",
    }
    .into();
    let post = &result.postprocess;
    if result.effective_level != Level::None {
        if !no_change || result.interrupted {
            report["bare_edges_resolved"] = post.bare_edges_resolved.into();
            report["cpp_scoped_edges_resolved"] = post.cpp_scoped_edges_resolved.into();
        }
        report["signatures_updated"] = true.into();
        report["fts_indexed"] = post.fts_indexed.into();
        report["fts_rebuilt"] = post.fts_delta.is_none().into();
        if result.effective_level == Level::Full {
            report["flows_detected"] = post.flows_detected.into();
            report["communities_detected"] = post.communities_detected.into();
        }
        let mut timing = json!({});
        for key in ["signatures_s", "fts_s", "flows_s", "communities_s"] {
            if let Some(v) = post.timing.get(key) {
                timing[key] = ((v * 1e6).round() / 1e6).into();
            }
        }
        report["postprocess_timing"] = timing;
    }
    if args["embedding_provider"].is_string() && args["embedding_model"].is_string() {
        report["embeddings_refresh_skipped"] = true.into();
        report["warnings"] = json!([
            "Embedding refresh skipped: this graph has no embeddings yet. Run `code-review-graph embed` (MCP: embed_graph_tool) once with the same provider and model to create them."
        ]);
    }
    if !(no_change && level == Level::None) {
        report["repaired_interrupted_build"] = result.interrupted.into();
    }
    Ok(report)
}

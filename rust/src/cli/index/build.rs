use super::store;
use crate::cli::wire;
use anyhow::{Result, ensure};
use code_review_graph_rs::canonical::{self, postprocess::Level};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::Path;

pub(in crate::cli) fn contended(db_path: &Path, outcome: &str) -> Value {
    let lock = db_path.with_file_name("write.lock");
    let pid = std::fs::read_to_string(&lock)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());
    let holder = pid.map_or_else(|| "another process".into(), |n| format!("PID {n}"));
    json!({"status":"contended","skipped":true,"build_type":"none","holder_pid":pid,"lock_path":lock,
                "files_parsed":0,"files_updated":0,"total_nodes":0,"total_edges":0,"errors":[],
                "summary":format!("Another code-review-graph write is already running for this repository ({holder}); {outcome}. Pass wait_for_lock=True (CLI: --wait) to block until it finishes.")})
}

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
    let level = match args["postprocess"].as_str().unwrap_or("full") {
        "full" => Level::Full,
        "minimal" => Level::Minimal,
        "none" => Level::None,
        other => anyhow::bail!("invalid postprocess level: {other}"),
    };
    let db_path = store::database(root, None);
    let sources = canonical::Sources::Build {
        quiet: false,
        follow: None,
        base: args["base"].as_str(),
        recurse_submodules: args["recurse_submodules"].as_bool(),
    };
    let result = match canonical::index_sources(
        root,
        &db_path,
        args["full_rebuild"].as_bool().unwrap_or(false),
        level,
        false,
        refresh_request(args),
        sources,
    ) {
        Err(e) if e.to_string() == "another writer is using this graph" => {
            return Ok(contended(&db_path, "skipped instead of repeating the work"));
        }
        Err(e) if e.is::<canonical::DiscoveryError>() => {
            let err = e.downcast::<canonical::DiscoveryError>()?;
            return Ok(
                json!({"status":"error","build_type":"incremental","base_resolved":err.base,
                "files_updated":0,"errors":[],"error":err.reason,"summary":err.to_string(),
                "postprocess_level":args["postprocess"].as_str().unwrap_or("full")}),
            );
        }
        other => other?,
    };
    let db = store::open(&db_path)?;
    let no_change = !result.full && result.files_parsed == 0;
    let mut report = if result.full {
        json!({"files_parsed":result.files_parsed,"stale_files_removed":result.files_deleted,"total_nodes":result.nodes,"total_edges":result.edges,"errors":[]})
    } else {
        let mut value = json!({"files_updated":result.files_parsed,"total_nodes":result.updated_nodes,
            "total_edges":result.updated_edges,"changed_files":result.changed_files,"dependent_files":result.dependent_files,"stale_files_removed":result.files_deleted});
        if no_change {
            value["flow_entry_point_qns"] = json!([]);
        }
        value["communities_were_affected"] = result.communities_affected.into();
        value["errors"] = json!([]);
        value["freshness_advanced"] = result.freshness_advanced.into();
        value
    };
    if result.full {
        full_resolvers(&mut report, &db)?;
    } else if !result.changed_files.is_empty() || result.freshness_advanced || !no_change {
        let mut available = json!({});
        let updated = result
            .changed_files
            .iter()
            .chain(&result.dependent_files)
            .collect::<Vec<_>>();
        for (name, extensions) in [
            ("python_resolution", &["py"][..]),
            ("rescript_resolution", &["res", "resi"]),
            ("spring_resolution", &["java"]),
            ("aop_resolution", &["java"]),
            ("event_resolution", &["java"]),
            ("temporal_resolution", &["java"]),
            ("hcl_resolution", &["tf", "hcl"]),
            ("scoped_resolution", &["php", "rs", "cs", "csproj"]),
            ("jedi_enrichment", &["py"]),
        ] {
            let relevant = !no_change
                && updated.iter().any(|p| {
                    Path::new(p)
                        .extension()
                        .and_then(|s| s.to_str())
                        .is_some_and(|ext| extensions.contains(&ext))
                        && (name != "jedi_enrichment" || root.join(p).is_file())
                });
            if relevant && available.as_object().is_some_and(|o| o.is_empty()) {
                full_resolvers(&mut available, &db)?;
            }
            report[name] = if relevant {
                available[name].take()
            } else {
                Value::Null
            };
        }
    }
    report["status"] = "ok".into();
    report["build_type"] = if result.full { "full" } else { "incremental" }.into();
    report["base_resolved"] = json!(result.base);
    report["summary"]=if result.full {
        format!("Full build complete: parsed {} files, created {} nodes and {} edges.",result.files_parsed,result.nodes,result.edges)
    } else if no_change && result.freshness_advanced {"No changes detected. Graph is up to date.".into()}
    else if no_change {"No graph changes detected. Freshness metadata was not advanced.".into()}
    else {format!("Incremental update: {} files re-parsed, {} nodes and {} edges updated. Changed: {}. Dependents also updated: {}.",
        result.files_parsed,result.updated_nodes,result.updated_edges,wire::value_repr(&report["changed_files"]),wire::value_repr(&report["dependent_files"]))}.into();
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
    let messages = refresh_result(&mut report, &result.embeddings, args, false);
    if !messages.is_empty() {
        report["warnings"] = json!(messages);
    }
    if !(no_change && level == Level::None) {
        report["repaired_interrupted_build"] = result.interrupted.into();
    }
    Ok(report)
}

const ABSENT_EMBEDDINGS: &str = "Embedding refresh skipped: this graph has no embeddings yet. Run `code-review-graph embed` (MCP: embed_graph_tool) once with the same provider and model to create them.";
const INCOMPLETE: &str = "The last build stopped before every file was stored, so the graph is still incomplete. Post-processing rebuilt what it could from the stored nodes; run 'code-review-graph build' to finish the graph itself.";

pub(in crate::cli) fn postprocess(
    root: &Path,
    db: &Path,
    args: &Value,
    wait: bool,
    verbose: bool,
) -> Result<Value> {
    let included = |name: &str| args[name].as_bool().unwrap_or(true);
    let result = match canonical::postprocess::rerun_with_embeddings(
        db,
        root,
        included("flows"),
        included("communities"),
        included("fts"),
        wait,
        refresh_request(args),
    ) {
        Err(e) if e.to_string() == "another writer is using this graph" => {
            return Ok(contended(db, "skipped instead of repeating the work"));
        }
        other => other?,
    };
    let post = result.computed;
    let mut report = json!({"status":"ok","bare_edges_resolved":post.bare_edges_resolved,"cpp_scoped_edges_resolved":post.cpp_scoped_edges_resolved,"signatures_updated":true});
    if verbose && result.new_db {
        eprintln!("INFO: Created graph database (schema v15).");
    }
    if included("fts") {
        report["fts_indexed"] = post.fts_indexed.into();
        if verbose {
            eprintln!("INFO: FTS index rebuilt: {} rows indexed", post.fts_indexed);
        }
    }
    if included("flows") {
        report["flows_detected"] = post.flows_detected.into();
    }
    if included("communities") {
        report["communities_detected"] = post.communities_detected.into();
        if verbose {
            eprintln!(
                "INFO: Tip: pip install \"code-review-graph[communities]\" for finer communities (grouping by file for now)."
            );
        }
    }
    let mut warnings = refresh_result(&mut report, &result.embeddings, args, verbose);
    if result.incomplete {
        warnings.push(INCOMPLETE.into());
        if verbose {
            eprintln!("WARNING: {INCOMPLETE}");
        }
        report["build_incomplete"] = true.into();
        report["status"] = "partial".into();
        report["summary"] = "Post-processing ran, but the graph is missing files from an unfinished build. Run a full build to repair it.".into();
    } else {
        report["summary"] = "Post-processing complete.".into();
    }
    if !warnings.is_empty() {
        report["warnings"] = json!(warnings);
    }
    Ok(report)
}

pub(in crate::cli) fn postprocess_cli(
    root: &Path,
    db: &Path,
    args: &Value,
    wait: bool,
) -> Result<()> {
    let report = postprocess(root, db, args, wait, true)?;
    if report["status"] == "contended" {
        eprintln!(
            "Skipped: {}",
            report["summary"].as_str().unwrap_or_default()
        );
        std::process::exit(75);
    }
    let mut parts = Vec::new();
    for (field, label) in [
        ("flows_detected", "flows"),
        ("communities_detected", "communities"),
        ("fts_indexed", "FTS entries"),
    ] {
        if let Some(n) = report[field].as_u64().filter(|v| *v > 0) {
            parts.push(format!("{n} {label}"));
        }
    }
    println!(
        "Post-processing: {}",
        if parts.is_empty() {
            "done".to_owned()
        } else {
            parts.join(", ")
        }
    );
    if report["build_incomplete"] == true {
        println!(
            "Build state: INCOMPLETE - the last build stopped before every file was stored, so files are still missing from the graph. Post-processing cannot add them. Run 'code-review-graph build' to rebuild."
        );
    }
    Ok(())
}

pub(in crate::cli) fn forget_cli(
    root: &Path,
    path: &Path,
    patterns: &[String],
    preview: bool,
    wait: bool,
) -> Result<()> {
    let outcome = match canonical::forget::run(root, path, patterns, preview, wait) {
        Err(e) if e.to_string() == "another writer is using this graph" => {
            let result = contended(path, "nothing was forgotten");
            eprintln!(
                "Skipped: {}",
                result["summary"].as_str().unwrap_or_default()
            );
            std::process::exit(75);
        }
        other => other?,
    };
    if outcome.files.is_empty() {
        println!(
            "No parsed files matched the given path(s).\nThe graph currently tracks {} file(s).",
            outcome.tracked
        );
        return Ok(());
    }
    println!(
        "{}",
        if preview {
            "[dry-run] Would forget these files:"
        } else {
            "Forgetting these files:"
        }
    );
    for name in &outcome.files {
        let file = Path::new(name);
        println!("  {}", file.strip_prefix(root).unwrap_or(file).display());
    }
    if preview {
        println!(
            "\n[dry-run] {} file(s) would be removed from the graph. No changes made.",
            outcome.files.len()
        );
    } else {
        if outcome.reparsed > 0 {
            println!(
                "  re-resolved {} referring file(s) so no edges dangle",
                outcome.reparsed
            );
        }
        println!(
            "\nForgot {} file(s); {} file(s) remain in the graph.",
            outcome.files.len(),
            outcome.tracked - outcome.files.len()
        );
        if let Some(post) = outcome.derived {
            match post.fts_delta {
                Some((a, b)) if a + b > 0 => {
                    eprintln!("INFO: FTS index delta: {a} entries removed, {b} indexed")
                }
                None => eprintln!("INFO: FTS index rebuilt: {} rows indexed", post.fts_indexed),
                _ => {}
            }
            eprintln!(
                "INFO: Tip: pip install \"code-review-graph[communities]\" for finer communities (grouping by file for now)."
            );
        }
    }
    Ok(())
}

pub fn embed(
    root: &Path,
    db: &Path,
    provider: Option<&str>,
    model: Option<&str>,
    wait: bool,
) -> Result<Value> {
    ensure!(
        root.is_dir(),
        "repo_root is not an existing directory: {}",
        root.display()
    );
    ensure!(
        [".git", ".svn", ".code-review-graph"]
            .iter()
            .any(|marker| root.join(marker).exists()),
        "repo_root does not look like a project root (no .git, .svn, or .code-review-graph directory found): {}",
        root.display()
    );
    match canonical::embeddings::write(db, provider, model, wait, |conn| {
        store::check_root(conn, db, root)
    }) {
        Err(e) if e.to_string() == "another writer is using this graph" => {
            Ok(contended(db, "nothing was embedded"))
        }
        result => result,
    }
}

fn refresh_request(args: &Value) -> Option<(&str, &str)> {
    args["embedding_provider"]
        .as_str()
        .zip(args["embedding_model"].as_str())
}
pub(in crate::cli) fn refresh_result(
    report: &mut Value,
    outcome: &canonical::embeddings::Refresh,
    args: &Value,
    verbose: bool,
) -> Vec<String> {
    use canonical::embeddings::Refresh;
    let message = match outcome {
        Refresh::NotRequested
            if args["embedding_provider"].is_string() != args["embedding_model"].is_string() =>
        {
            let warning = "Embedding refresh requires both an explicit provider and model.";
            if verbose {
                eprintln!("WARNING: {warning}");
            }
            warning.into()
        }
        Refresh::NotRequested => return vec![],
        Refresh::Updated { embedded, purged } => {
            report["embeddings_refreshed"] = json!(embedded);
            report["embeddings_purged"] = json!(purged);
            return vec![];
        }
        Refresh::Empty => {
            report["embeddings_refresh_skipped"] = json!(true);
            ABSENT_EMBEDDINGS.into()
        }
        Refresh::Failed { class, reason } => format!("Embedding refresh failed: {class}: {reason}"),
    };
    if verbose {
        log_refresh(outcome);
    }
    vec![message]
}

pub(in crate::cli) fn log_refresh(outcome: &canonical::embeddings::Refresh) {
    match outcome {
        canonical::embeddings::Refresh::Empty => eprintln!("WARNING: {ABSENT_EMBEDDINGS}"),
        canonical::embeddings::Refresh::Failed { reason, .. } => {
            eprintln!("WARNING: Embedding refresh failed: {reason}")
        }
        _ => (),
    }
}

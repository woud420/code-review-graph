mod args;
mod build;
mod changes;
mod eval;
mod history;
mod mcp;
mod query;
mod registry;
mod reports;
mod search;
mod store;
use query::terms;
mod visualize;
mod wiki;
mod wire;

use anyhow::{Result, ensure};
use args::{Cli, Command};
use clap::CommandFactory;
use code_review_graph_rs::canonical;
use std::{
    env,
    path::{Path, PathBuf},
};

use code_review_graph_rs::registry::absolute;

fn root(path: Option<&Path>, boundary: bool) -> Result<PathBuf> {
    if let Some(path) = path {
        let path = absolute(path)?;
        if boundary {
            return store::project_root(&path).ok_or_else(||anyhow::anyhow!("--repo does not look like a project root (no .git, .svn, or .code-review-graph found at or above): {}",path.display()));
        }
        return Ok(path);
    }
    if let Some(val) = env::var_os("CRG_REPO_ROOT").filter(|s| !s.is_empty()) {
        return absolute(Path::new(&val));
    }
    let cwd = env::current_dir()?;
    Ok(store::project_root(&cwd).unwrap_or(cwd))
}

pub fn dispatch() -> Result<()> {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("daemon")) {
        std::process::exit(code_review_graph_rs::daemon::run(
            std::env::args_os().skip(2),
        ));
    }
    let Some(command) = args::parse().command else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };
    match command {
        Command::Eval { report, output_dir } => {
            if report {
                eval::report(&output_dir)
            } else {
                println!("Specify --all, --repo, or --benchmark. See --help.");
                Ok(())
            }
        }
        Command::DeadCode {
            location,
            kind,
            file_pattern,
            limit,
            json,
        } => {
            let given = root(location.repo.as_deref(), false)?;
            let root = store::project_root(&given).unwrap_or(given);
            let db = store::database(&root, location.data_dir.as_deref());
            let conn = store::open(&db)?;
            store::check_root(&conn, &db, &root)?;
            let mut items = reports::dead_code(
                &conn,
                &root,
                kind.as_deref().unwrap_or(""),
                file_pattern.as_deref().unwrap_or(""),
            )?;
            let total = items.len();
            eprintln!("INFO: find_dead_code: found {total} dead symbols");
            if limit > 0 {
                items.truncate(limit);
            }
            if json {
                println!("{}", wire::json_text(&items.into(), true));
            } else {
                println!("Dead code: {total} item(s); showing {}", items.len());
                for row in &items {
                    println!(
                        "  [{}] {}  ({}:{})",
                        query::text(row, "kind"),
                        query::text(row, "name"),
                        query::text(row, "relative_path"),
                        row["line"]
                    );
                }
            }
            Ok(())
        }
        Command::ContextHistory {
            location,
            by,
            days,
            json,
        } => {
            let given = root(location.repo.as_deref(), false)?;
            let root = store::project_root(&given).unwrap_or(given);
            let db = store::database(&root, location.data_dir.as_deref());
            history::print(db.with_file_name("context-history.jsonl"), &by, days, json)
        }
        Command::Repos => registry::print(),
        Command::Register { path, alias } => registry::register(&path, alias),
        Command::Unregister { path_or_alias } => registry::unregister(&path_or_alias),
        Command::Status {
            location,
            quiet,
            json,
        } => {
            let root = root(location.repo.as_deref(), false)?;
            let db = store::database(&root, location.data_dir.as_deref());
            store::status(&root, &store::open(&db)?, json, quiet)
        }
        Command::Query {
            pattern,
            target,
            repo,
            to,
        } => {
            if !terms::PATTERNS.iter().any(|(key, _)| *key == pattern) {
                let choices = terms::PATTERNS
                    .iter()
                    .map(|(p, _)| (*p).to_owned())
                    .collect::<Vec<_>>()
                    .join(", ");
                eprintln!(
                    "usage: code-review-graph query [-h] [--to TO] [--repo REPO] pattern target\ncode-review-graph query: error: argument pattern: invalid choice: '{pattern}' (choose from {choices})"
                );
                std::process::exit(2);
            }
            let root = root(repo.as_deref(), true)?;
            let db = store::database(&root, None);
            let conn = store::open(&db)?;
            store::check_root(&conn, &db, &root)?;
            let result = query::run(
                &conn,
                &root,
                &query::Query {
                    pattern,
                    target,
                    to,
                    ..Default::default()
                },
            )?;
            println!("{}", wire::json_text(&result, true));
            Ok(())
        }
        Command::Search {
            query,
            kind,
            limit,
            repo,
        } => {
            if limit < 1 {
                let message = if limit < 0 {
                    "must be zero or greater"
                } else {
                    "must be greater than zero"
                };
                eprintln!(
                    "usage: code-review-graph search [-h]\n                                [--kind {{File,Class,Function,Type,Test,YamlPath,DocSection,Variable,Output,JsonPath}}]\n                                [--limit LIMIT] [--repo REPO]\n                                query\ncode-review-graph search: error: argument --limit: {message}"
                );
                std::process::exit(2);
            }
            let root = root(repo.as_deref(), true)?;
            let db = store::database(&root, None);
            let conn = store::open(&db)?;
            store::check_root(&conn, &db, &root)?;
            let args = serde_json::from_value(
                serde_json::json!({"query":query,"kind":kind,"limit":limit}),
            )?;
            let data = search::run(&conn, &root, &args)?;
            println!("{}", wire::json_text(&data, true));
            Ok(())
        }
        Command::Refactor {
            mode,
            repo,
            old_name,
            new_name,
            kind,
            path,
        } => {
            if mode == "rename"
                && (old_name.as_deref().unwrap_or("").is_empty()
                    || new_name.as_deref().unwrap_or("").is_empty())
            {
                eprintln!(
                    "usage: code-review-graph refactor [-h] [--old-name OLD_NAME]\n                                  [--new-name NEW_NAME]\n                                  [--kind {{Function,Class}}] [--path PATH]\n                                  [--repo REPO]\n                                  {{rename,dead_code,suggest}}\ncode-review-graph refactor: error: rename requires --old-name and --new-name"
                );
                std::process::exit(2);
            }
            print_report(
                repo.as_deref(),
                "refactor_tool",
                serde_json::json!({"mode":mode,"old_name":old_name,"new_name":new_name,"kind":kind,"file_pattern":path}),
            )
        }
        Command::DetectChanges {
            repo,
            base,
            brief,
            churn,
            verify,
            details,
        } => {
            let root = root(repo.as_deref(), false)?;
            let db = store::database(&root, None);
            let conn = store::open(&db)?;
            store::check_root(&conn, &db, &root)?;
            reports::detect::print(
                &conn,
                &root,
                &base,
                brief,
                churn,
                verify,
                details.as_deref(),
            )
        }
        Command::Impact {
            repo,
            files,
            depth,
            max_results,
            max_files,
            base,
            detail_level,
        } => print_report(
            repo.as_deref(),
            "get_impact_radius_tool",
            serde_json::json!({"changed_files":files,"max_depth":depth,"max_results":max_results,"max_files":max_files,"base":base,"detail_level":detail_level}),
        ),
        Command::Architecture { repo, detail_level } => print_report(
            repo.as_deref(),
            "get_architecture_overview_tool",
            serde_json::json!({"detail_level":detail_level}),
        ),
        Command::LargeFunctions {
            repo,
            min_lines,
            kind,
            path,
            limit,
        } => print_report(
            repo.as_deref(),
            "find_large_functions_tool",
            serde_json::json!({"min_lines":min_lines,"kind":kind,"file_path_pattern":path,"limit":limit}),
        ),
        Command::Flows {
            repo,
            sort,
            limit,
            kind,
        } => print_report(
            repo.as_deref(),
            "list_flows_tool",
            serde_json::json!({"sort_by":sort,"limit":limit,"kind":kind}),
        ),
        Command::Flow {
            repo,
            id,
            name,
            source,
        } => print_report(
            repo.as_deref(),
            "get_flow_tool",
            serde_json::json!({"flow_id":id,"flow_name":name,"include_source":source}),
        ),
        Command::Communities {
            repo,
            sort,
            min_size,
        } => print_report(
            repo.as_deref(),
            "list_communities_tool",
            serde_json::json!({"sort_by":sort,"min_size":min_size}),
        ),
        Command::Community {
            repo,
            id,
            name,
            members,
        } => print_report(
            repo.as_deref(),
            "get_community_tool",
            serde_json::json!({"community_id":id,"community_name":name,"include_members":members}),
        ),
        Command::Serve { repo, tools } => mcp::serve(&root(repo.as_deref(), false)?, tools),
        Command::Visualize {
            location,
            mode,
            format,
            sidecar,
        } => {
            let root = root(location.repo.as_deref(), false)?;
            let db = store::database(&root, location.data_dir.as_deref());
            let conn = store::open(&db)?;
            store::check_root(&conn, &db, &root)?;
            visualize::write(&conn, &db, &format, &mode, sidecar)
        }
        Command::Wiki { location, force } => {
            let root = root(location.repo.as_deref(), false)?;
            let db = store::database(&root, location.data_dir.as_deref());
            let conn = store::open(&db)?;
            store::check_root(&conn, &db, &root)?;
            let response = wiki::write(&conn, &db, force)?;
            println!(
                "Wiki: {}",
                response["summary"]
                    .as_str()
                    .unwrap_or_default()
                    .trim_start_matches("Wiki generated: ")
            );
            println!("Output: {}", db.with_file_name("wiki").display());
            Ok(())
        }
        Command::Forget {
            location,
            paths,
            dry_run,
            wait,
        } => {
            let root = root(location.repo.as_deref(), false)?;
            let path = store::database(&root, location.data_dir.as_deref());
            build::forget_cli(&root, &path, &paths, dry_run, wait)
        }
        Command::Postprocess {
            location,
            no_flows,
            no_communities,
            no_fts,
            wait,
            embedding_provider,
            embedding_model,
        } => {
            if embedding_provider.is_some() != embedding_model.is_some() {
                eprintln!(
                    "usage: code-review-graph [-h] [-v] <command> ...\ncode-review-graph: error: --embedding-provider and --embedding-model must be supplied together"
                );
                std::process::exit(2);
            }
            let root = root(location.repo.as_deref(), false)?;
            let path = write_path(&root, location.data_dir.as_deref())?;
            let args = serde_json::json!({"flows":!no_flows,"communities":!no_communities,"fts":!no_fts,"embedding_provider":embedding_provider,"embedding_model":embedding_model});
            build::postprocess_cli(&root, &path, &args, wait)
        }
        Command::Build(ref args) | Command::Update(ref args) => {
            let full = matches!(command, Command::Build(_));
            let root = root(args.location.repo.as_deref(), false)?;
            if !full && args.location.repo.is_none() && store::project_root(&root).is_none() {
                eprintln!("ERROR: Not in a git repository. 'update' requires git for diffing.");
                eprintln!("ERROR: Use 'build' for a full parse, or run 'git init' first.");
                std::process::exit(1);
            }
            ensure!(
                root.is_dir(),
                "Error: --repo is not a folder: {}. Check the path and try again.",
                root.display()
            );
            use canonical::postprocess::Level;
            let (level, postprocess) = if args.skip_postprocess {
                (Level::None, "none")
            } else if args.skip_flows {
                (Level::Minimal, "minimal")
            } else {
                (Level::Full, "full")
            };
            let db = write_path(&root, args.location.data_dir.as_deref())?;
            let new_db = !db.exists();
            let report = match canonical::index(&root, &db, full, level, args.wait) {
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::WouldBlock)
                        || error.to_string() == "another writer is using this graph" =>
                {
                    let holder = db
                        .parent()
                        .and_then(|p| std::fs::read_to_string(p.join("write.lock")).ok())
                        .and_then(|s| s.trim().parse::<u32>().ok())
                        .map(|n| format!("PID {n}"))
                        .unwrap_or_else(|| "another process".to_owned());
                    eprintln!(
                        "Skipped: Another code-review-graph write is already running for this repository ({holder}); skipped instead of repeating the work. Pass wait_for_lock=True (CLI: --wait) to block until it finishes."
                    );
                    std::process::exit(75);
                }
                other => other?,
            };
            if !args.quiet {
                if new_db {
                    eprintln!("INFO: Created graph database (schema v15).");
                }
                if !full
                    && !report.full
                    && !root
                        .ancestors()
                        .any(|dir| dir.join(".git").exists() || dir.join(".svn").exists())
                {
                    eprintln!("WARNING: git status failed while discovering working-tree files");
                }
                if report.full {
                    let total = report.files_parsed;
                    let interval = if total < 8
                        || std::env::var("CRG_PARSE_EXECUTOR").as_deref() == Ok("serial")
                    {
                        50
                    } else {
                        200
                    };
                    for i in (interval..total).step_by(interval) {
                        eprintln!("INFO: Progress: {i}/{total} files parsed");
                    }
                    if total > 0 {
                        eprintln!("INFO: Progress: {total}/{total} files parsed");
                    }
                }
                match report.postprocess.fts_delta {
                    Some((removed, added)) if removed + added > 0 => eprintln!(
                        "INFO: FTS index delta: {removed} entries removed, {added} indexed"
                    ),
                    None if level != Level::None
                        && (report.full || report.postprocess.fts_indexed > 0) =>
                    {
                        eprintln!(
                            "INFO: FTS index rebuilt: {} rows indexed",
                            report.postprocess.fts_indexed
                        )
                    }
                    _ => {}
                }
                if level == Level::Full && report.postprocess.communities_detected > 0 {
                    eprintln!(
                        "INFO: Tip: pip install \"code-review-graph[communities]\" for finer communities (grouping by file for now)."
                    );
                }
                if full {
                    println!(
                        "Full build: {} files, {} nodes, {} edges (postprocess={postprocess})",
                        report.files_parsed, report.nodes, report.edges
                    );
                    if report.files_parsed == 0 {
                        eprintln!(
                            "Warning: no source files found under {}. Is this the project root?",
                            root.display()
                        );
                    } else {
                        println!(
                            "Next: code-review-graph detect-changes --brief, or ask your assistant \"Review my changes\"."
                        );
                    }
                } else if report.full {
                    println!(
                        "Full rebuild (no usable incremental base): {} files, {} nodes, {} edges (postprocess={postprocess})",
                        report.files_parsed, report.nodes, report.edges
                    );
                } else {
                    println!(
                        "Incremental: {} files updated, {} nodes, {} edges (postprocess={postprocess})",
                        report.files_parsed,
                        if report.files_parsed == 0 && report.files_deleted == 0 {
                            0
                        } else {
                            report.nodes
                        },
                        if report.files_parsed == 0 && report.files_deleted == 0 {
                            0
                        } else {
                            report.edges
                        }
                    );
                }
            }
            Ok(())
        }
    }
}

fn write_path(root: &Path, dir: Option<&Path>) -> Result<PathBuf> {
    ensure!(
        root.is_dir(),
        "Error: --repo is not a folder: {}. Check the path and try again.",
        root.display()
    );
    let db = store::database(root, dir);
    canonical::ensure_writable(&db)?;
    if let Some(dir) = dir {
        let dir = absolute(dir)?;
        code_review_graph_rs::registry::set_data_dir(root, &dir)?;
        eprintln!("INFO: Graph database will be stored at: {}", dir.display());
    }
    if ![".git", ".svn", ".code-review-graph"]
        .iter()
        .any(|p| root.join(p).exists())
    {
        std::fs::create_dir_all(root.join(".code-review-graph"))?;
    }
    Ok(db)
}

fn print_report(repo: Option<&Path>, name: &str, args: serde_json::Value) -> Result<()> {
    let root = root(repo, true)?;
    let db = store::database(&root, None);
    let conn = store::open(&db)?;
    store::check_root(&conn, &db, &root)?;
    let label = match name {
        "get_architecture_overview_tool" => Some("architecture"),
        "get_impact_radius_tool" => Some("impact"),
        _ => None,
    };
    let data = history::with_label(label, || {
        reports::run(&conn, &root, name, &args, &reports::Hints::default())
    })?;
    println!("{}", wire::json_text(&data, true));
    Ok(())
}

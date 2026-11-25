mod graph;
mod mcp;
mod model;
mod parser;
mod typescript;

use std::{io, path::PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use graph::Graph;
use serde_json::{Value, json};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[arg(long, global = true, default_value = ".")]
    repo: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve read-only graph queries over MCP on stdin and stdout.
    Serve,
    /// Parse every supported source file and replace the graph.
    Build,
    /// Refresh the graph from files added, changed or removed on disk.
    Update,
    /// Count indexed files, symbols and resolved call edges.
    Status,
    /// Inspect indexed symbols and their resolved calls.
    Query {
        #[command(subcommand)]
        query: Query,
    },
}

#[derive(Subcommand)]
enum Query {
    /// Search symbol names or look up a file::name.
    Symbols { target: String },
    /// List the direct callers of a symbol.
    CallersOf { target: String },
    /// List the symbols called directly by a symbol.
    CalleesOf { target: String },
    /// Follow incoming calls up to the requested depth.
    Impact {
        target: String,
        #[arg(long, default_value_t = 3)]
        depth: usize,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.repo.canonicalize().context("opening repository")?;
    anyhow::ensure!(root.is_dir(), "expected a repository directory");
    let dir = root.join(".code-review-graph-rs");
    let database = dir.join("graph.db");
    let open_index = || {
        Graph::open_existing(&database)
            .context("opening graph for reading (run build to create an index)")
    };
    let response: Value = match cli.command {
        Command::Serve => return mcp::serve(&database),
        Command::Build | Command::Update => {
            let mut graph = Graph::open(&database)?;
            json!(graph.sync(&root, matches!(cli.command, Command::Build))?)
        }
        Command::Status => json!(open_index()?.stats()?),
        Command::Query { query } => {
            let graph = open_index()?;
            let results = match query {
                Query::Symbols { target } => graph.symbols(&target)?,
                Query::CallersOf { target } => graph.callers(&target)?,
                Query::CalleesOf { target } => graph.callees(&target)?,
                Query::Impact { target, depth } => graph.impact(&target, depth)?,
            };
            json!({ "results": results })
        }
    };
    serde_json::to_writer(io::stdout().lock(), &response)?;
    println!();
    Ok(())
}

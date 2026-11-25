use super::index::location::{FollowSymlinks, Location};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "code-review-graph",
    version = "2.4.0",
    disable_version_flag = true,
    disable_help_subcommand = true,
    about = "Read an upstream code graph and index supported source files in Rust"
)]
pub struct Cli {
    #[arg(short='v', long, action=clap::ArgAction::Version)]
    pub version: Option<bool>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Args)]
pub struct Write {
    #[command(flatten)]
    pub links: FollowSymlinks,
    #[arg(long)]
    pub enrich: bool,
    #[arg(long,value_parser=["local","openai","google","minimax","voyage"])]
    pub embedding_provider: Option<String>,
    #[arg(long)]
    pub embedding_model: Option<String>,

    #[command(flatten)]
    pub location: Location,
    #[arg(short, long)]
    pub quiet: bool,
    /// Wait for an existing writer to finish.
    #[arg(long)]
    pub wait: bool,
    /// Omit flow and community detection.
    #[arg(long)]
    pub skip_flows: bool,
    /// Omit all post-processing.
    #[arg(long)]
    pub skip_postprocess: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Register the native graph server, hooks and skills.
    #[command(visible_alias = "init")]
    Install(super::install::Options),
    /// Remove a platform's graph server registration.
    Uninstall(super::install::Uninstall),
    #[command(hide = true)]
    Readonly {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<std::ffi::OsString>,
    },
    /// Compute vector embeddings for semantic search.
    Embed {
        #[command(flatten)]
        location: Location,
        #[arg(long,value_parser=["local","openai","google","minimax","voyage"])]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        wait: bool,
    },
    #[command(hide = true)]
    HookUpdate {
        #[arg(long)]
        existing_only: bool,
    },
    #[command(hide = true)]
    HookStatus,
    #[command(hide = true)]
    Enrich,
    /// List registered repositories.
    Repos,
    /// Register a local repository.
    Register {
        path: PathBuf,
        #[arg(long)]
        alias: Option<String>,
    },
    /// Remove a registered repository.
    Unregister { path_or_alias: String },
    /// Show the opt-in context savings log.
    ContextHistory {
        #[command(flatten)]
        location: Location,
        #[arg(long, default_value = "day", value_parser = ["day", "tool"])]
        by: String,
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
        days: Option<u32>,
        #[arg(long)]
        json: bool,
    },
    /// Diagnose the graph, installed copies and hooks.
    Doctor {
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Show graph statistics.
    Status {
        #[command(flatten)]
        location: Location,
        #[arg(short, long)]
        quiet: bool,
        #[arg(long)]
        json: bool,
    },
    /// Query relationships in an existing graph.
    Query {
        pattern: String,
        target: String,
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        to: Option<String>,
    },
    /// Export a graph visualization.
    Visualize(super::visualize::Options),
    /// Generate Markdown pages for graph communities.
    Wiki {
        #[command(flatten)]
        location: Location,
        #[arg(long)]
        force: bool,
    },
    /// Remove indexed files and recompute their relationships.
    Forget {
        #[command(flatten)]
        location: Location,
        #[arg(required = true, num_args=1..)]
        paths: Vec<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        wait: bool,
    },
    /// Run post-processing on the stored graph.
    Postprocess {
        #[command(flatten)]
        location: Location,
        #[arg(long)]
        no_flows: bool,
        #[arg(long)]
        no_communities: bool,
        #[arg(long)]
        no_fts: bool,
        #[arg(long)]
        wait: bool,
        #[arg(long, value_parser = ["local","openai","google","minimax","voyage"])]
        embedding_provider: Option<String>,
        #[arg(long)]
        embedding_model: Option<String>,
    },
    /// Full raw graph build.
    Build(Write),
    /// Incrementally update the raw graph.
    Update(Write),
    /// Search graph entities by name or keyword.
    Search {
        query: String,
        #[arg(long,value_parser=["File","Class","Function","Type","Test","YamlPath","DocSection","Variable","Output","JsonPath"])]
        kind: Option<String>,
        #[arg(long, default_value_t = 20, allow_hyphen_values = true)]
        limit: i64,
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Find symbols without callers or test references.
    DeadCode {
        #[command(flatten)]
        location: Location,
        #[arg(long, value_parser=["Function","Class"])]
        kind: Option<String>,
        #[arg(long)]
        file_pattern: Option<String>,
        #[arg(long, default_value_t = 0)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Generate a report from evaluation results.
    Eval {
        #[arg(long)]
        report: bool,
        #[arg(long, default_value = "evaluate/results")]
        output_dir: PathBuf,
    },
    /// Preview graph-backed refactors.
    Refactor {
        #[arg(value_parser=["rename","dead_code","suggest"])]
        mode: String,
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        old_name: Option<String>,
        #[arg(long)]
        new_name: Option<String>,
        #[arg(long,value_parser=["Function","Class"])]
        kind: Option<String>,
        #[arg(long)]
        path: Option<String>,
    },
    /// Analyse change impact against the stored graph.
    DetectChanges {
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long, default_value = "HEAD~1")]
        base: String,
        #[arg(long)]
        brief: bool,
        #[arg(long)]
        churn: bool,
        #[arg(long)]
        verify: bool,
        #[arg(long)]
        details: Option<String>,
    },
    /// Analyse the blast radius of changes.
    Impact {
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long,num_args=1..)]
        files: Option<Vec<String>>,
        #[arg(long,default_value_t=2,value_parser=clap::value_parser!(i64).range(0..))]
        depth: i64,
        #[arg(long,default_value_t=500,value_parser=clap::value_parser!(i64).range(1..))]
        max_results: i64,
        #[arg(long,default_value_t=25,value_parser=clap::value_parser!(i64).range(1..))]
        max_files: i64,
        #[arg(long, default_value = "HEAD~1")]
        base: String,
        #[arg(long,default_value="standard",value_parser=["standard","minimal","compact"])]
        detail_level: String,
    },
    /// Show architecture and cross-community coupling.
    Architecture {
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long,default_value="minimal",value_parser=["minimal","standard"])]
        detail_level: String,
    },
    /// Find oversized graph nodes.
    LargeFunctions {
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long,default_value_t=50,value_parser=clap::value_parser!(i64).range(1..))]
        min_lines: i64,
        #[arg(long,value_parser=["Function","Class","File","Test"])]
        kind: Option<String>,
        #[arg(long)]
        path: Option<String>,
        #[arg(long,default_value_t=50,value_parser=clap::value_parser!(i64).range(1..))]
        limit: i64,
    },
    /// List stored execution flows.
    Flows {
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long,default_value="criticality",value_parser=["criticality","depth","node_count","file_count","name"])]
        sort: String,
        #[arg(long,default_value_t=50,value_parser=clap::value_parser!(i64).range(1..))]
        limit: i64,
        #[arg(long)]
        kind: Option<String>,
    },
    /// Show one stored execution flow.
    Flow {
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long,conflicts_with="name",required_unless_present="name",value_parser=clap::value_parser!(i64).range(1..))]
        id: Option<i64>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        source: bool,
    },
    /// List graph communities.
    Communities {
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long,default_value="size",value_parser=["size","cohesion","name"])]
        sort: String,
        #[arg(long,default_value_t=0,value_parser=clap::value_parser!(i64).range(0..))]
        min_size: i64,
    },
    /// Show one graph community.
    Community {
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long,conflicts_with="name",required_unless_present="name",value_parser=clap::value_parser!(i64).range(1..))]
        id: Option<i64>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        members: bool,
    },
    /// Watch for changes and auto-update.
    Watch(super::watch::Options),
    /// Run the MCP server over stdio or HTTP.
    Serve {
        #[command(flatten)]
        http: super::mcp::http::Options,
        #[arg(long)]
        auto_watch: bool,
        #[arg(long)]
        multi_worktree: bool,
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        tools: Option<String>,
    },
    /// Alias for the stdio MCP server.
    Mcp {
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        auto_watch: bool,
        #[arg(long)]
        multi_worktree: bool,
    },
}

pub fn parse() -> Cli {
    use clap::error::{ContextKind, ErrorKind};
    let argv: Vec<_> = std::env::args_os().collect();
    match Cli::try_parse_from(&argv) {
        Ok(value) => value,
        Err(error) => {
            let context = |key| error.get(key).map(ToString::to_string).unwrap_or_default();
            match error.kind() {
                ErrorKind::DisplayVersion => error.exit(),
                ErrorKind::DisplayHelp => {
                    // Keep the documented argparse layout for the commands that accept the same options.
                    let pages: std::collections::BTreeMap<String, String> =
                        serde_json::from_str(include_str!("../../assets/cli/help.json"))
                            .expect("bundled CLI help");
                    if let Some(page) = argv
                        .get(1)
                        .and_then(|s| s.to_str())
                        .and_then(|cmd| pages.get(if cmd == "-h" { "--help" } else { cmd }))
                    {
                        print!("{page}");
                        std::process::exit(0);
                    }
                    let mut help = error.render().to_string().replace("Usage:", "usage:");
                    if argv.get(1).is_some_and(|v| v == "init") {
                        help = help.replace(
                            "usage: code-review-graph install",
                            "usage: code-review-graph init",
                        );
                    }
                    print!("{help}");
                    std::process::exit(0);
                }
                ErrorKind::UnknownArgument if context(ContextKind::InvalidArg).starts_with('-') => {
                    let flag = context(ContextKind::InvalidArg);
                    // argparse reports the extra arguments at the root parser.
                    // A suggestion is advisory in clap, so keep the actual token.
                    eprintln!(
                        "usage: code-review-graph [-h] [-v] <command> ...\ncode-review-graph: error: unrecognized arguments: {flag}"
                    );
                }
                ErrorKind::InvalidSubcommand => {
                    let word = context(ContextKind::InvalidSubcommand);
                    let close = context(ContextKind::SuggestedSubcommand);
                    let hint = if close.is_empty() {
                        String::new()
                    } else {
                        format!(" Did you mean {close}?")
                    };
                    eprintln!(
                        "usage: code-review-graph [-h] [-v] <command> ...\ncode-review-graph: error: unknown command '{word}'.{hint} Run code-review-graph --help to see every command."
                    );
                }
                ErrorKind::MissingRequiredArgument if argv.get(1).is_some_and(|v| v == "query") => {
                    let missing = context(ContextKind::InvalidArg);
                    let fields = ["pattern", "target"]
                        .into_iter()
                        .filter(|field| missing.to_lowercase().contains(field))
                        .collect::<Vec<_>>()
                        .join(", ");
                    eprintln!(
                        "usage: code-review-graph query [-h] [--to TO] [--repo REPO] pattern target\ncode-review-graph query: error: the following arguments are required: {fields}"
                    );
                }
                ErrorKind::InvalidValue => {
                    use clap::CommandFactory;
                    let mut root = Cli::command();
                    root.build();
                    let usage = argv
                        .get(1)
                        .and_then(|s| s.to_str())
                        .and_then(|name| root.find_subcommand_mut(name))
                        .map(|cmd| cmd.render_usage().to_string())
                        .unwrap_or_else(|| root.render_usage().to_string());
                    eprintln!("{}", usage.replace("Usage:", "usage:"));
                    eprint!("{error}");
                }
                _ => {
                    eprint!("{}", error.render().to_string().replace("Usage:", "usage:"));
                    std::process::exit(error.exit_code());
                }
            }
            std::process::exit(2);
        }
    }
}

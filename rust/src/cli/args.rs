use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "code-review-graph",
    version = "2.4.0",
    disable_version_flag = true,
    about = "Read an upstream code graph and index supported source files in Rust"
)]
pub struct Cli {
    #[arg(short='v', long, action=clap::ArgAction::Version)]
    pub version: Option<bool>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Args)]
pub struct Location {
    #[arg(long)]
    pub repo: Option<PathBuf>,
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
}

#[derive(Args)]
pub struct Write {
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
    /// Run the local MCP server over stdio.
    #[command(visible_alias = "mcp")]
    Serve {
        #[arg(long)]
        repo: Option<PathBuf>,
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
                _ => error.exit(),
            }
            std::process::exit(2);
        }
    }
}

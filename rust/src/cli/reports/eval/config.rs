use serde::Deserialize;

#[derive(clap::Args)]
pub(super) struct Embeddings {
    #[arg(long)]
    pub(super) embed: bool,
    #[arg(long, value_parser = ["local","openai","google","minimax","voyage"])]
    pub(super) embed_provider: Option<String>,
    #[arg(long)]
    pub(super) embed_model: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct Config {
    pub(super) name: String,
    pub(super) url: String,
    pub(super) commit: String,
    #[serde(default)]
    pub(super) fidelity_edits: Option<Vec<String>>,
    #[serde(default)]
    pub(super) agent_questions: Vec<String>,
    #[serde(default)]
    pub(super) agent_baseline_top_k: Option<i64>,
    #[serde(default)]
    pub(super) multi_hop_tasks: Vec<Task>,
    #[serde(default)]
    pub(super) entry_points: Vec<String>,
    #[serde(default)]
    pub(super) search_queries: Vec<SearchQuery>,
    #[serde(default)]
    pub(super) test_commits: Vec<Commit>,
}

#[derive(Deserialize)]
pub(super) struct Commit {
    pub(super) sha: String,
    #[serde(default)]
    pub(super) description: String,
}

#[derive(Deserialize)]
pub(super) struct SearchQuery {
    pub(super) query: String,
    pub(super) expected: String,
}

#[derive(Deserialize)]
pub(super) struct Task {
    pub(super) id: String,
    pub(super) nl_query: String,
    pub(super) anchor_qualified_suffix: String,
    #[serde(default)]
    pub(super) traversal_pattern: Option<String>,
    #[serde(default)]
    pub(super) expected_neighbor_names: Vec<String>,
    #[serde(default)]
    pub(super) k: Option<i64>,
}

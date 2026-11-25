use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rmcp::{
    ServiceExt, handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool,
    tool_router, transport::stdio,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::graph::Graph;

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Pattern {
    CallersOf,
    CalleesOf,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Query {
    pattern: Pattern,
    /// Exact symbol name, repository-relative file, or file::name. A bare name combines all matches.
    target: String,
}

struct GraphServer {
    database: PathBuf,
}

impl GraphServer {
    async fn read(
        &self,
        query: impl FnOnce(Graph) -> Result<Value> + Send + 'static,
    ) -> CallToolResult {
        let path = self.database.clone();
        let result = tokio::task::spawn_blocking(move || {
            let graph =
                Graph::open_existing(&path).context("opening index; run build to create it")?;
            query(graph)
        })
        .await
        .context("graph reader stopped")
        .and_then(|result| result);
        match result {
            Ok(value) => CallToolResult::structured(value),
            Err(error) => {
                CallToolResult::structured_error(json!({ "error": format!("{error:#}") }))
            }
        }
    }
}

#[tool_router(server_handler)]
impl GraphServer {
    /// Query distinct resolved caller or callee symbols in this repository's existing index.
    /// Each result gives the symbol's source range. Repeated calls from one symbol appear once.
    #[tool(annotations(read_only_hint = true))]
    async fn query_graph_tool(&self, Parameters(query): Parameters<Query>) -> CallToolResult {
        self.read(move |graph| {
            let hits = match query.pattern {
                Pattern::CallersOf => graph.callers(&query.target)?,
                Pattern::CalleesOf => graph.callees(&query.target)?,
            };
            Ok(json!({ "results": hits }))
        })
        .await
    }

    /// Count files, symbols and resolved call edges in this repository's existing Rust index.
    #[tool(annotations(read_only_hint = true))]
    async fn list_graph_stats_tool(&self) -> CallToolResult {
        self.read(|graph| Ok(serde_json::to_value(graph.stats()?)?))
            .await
    }
}

pub fn serve(database: &Path) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let service = GraphServer {
            database: database.to_path_buf(),
        }
        .serve(stdio())
        .await?;
        service.waiting().await?;
        Ok(())
    })
}

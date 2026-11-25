use super::{prompts, query, registry, reports, search, store, wiki, wire};
use anyhow::{Result, anyhow};
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt, model::*, service::RequestContext,
    transport::stdio,
};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::{
    fs,
    path::{Path, PathBuf},
};

struct Server {
    hints: Arc<reports::Hints>,
    root: PathBuf,
    tools: Vec<Tool>,
}

fn docs(section: &str, root: &Path) -> Value {
    let override_docs = fs::read(root.join("docs/LLM-OPTIMIZED-REFERENCE.md"))
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    let packaged = include_str!("../../assets/LLM-OPTIMIZED-REFERENCE.md");
    let opening = format!("<section name=\"{}\">", section.to_ascii_lowercase());
    for content in override_docs.as_deref().into_iter().chain([packaged]) {
        let lower = content.to_ascii_lowercase();
        if let Some(start) = lower.find(&opening) {
            let body = start + opening.len();
            if let Some(end) = lower[body..].find("</section>") {
                return json!({"status":"ok","section":section,"content":content[body..body+end].trim()});
            }
        }
    }
    json!({"status":"not_found","error":format!("Section '{section}' not found. Available: usage, review-delta, review-pr, commands, legal, watch, embeddings, languages, troubleshooting")})
}

fn call(
    root: &Path,
    name: &str,
    args: Map<String, Value>,
    hints: &reports::Hints,
) -> Result<Value> {
    if name == "cross_repo_search_tool" {
        return registry::cross_search(&Value::Object(args));
    }
    if name == "list_repos_tool" {
        let repos = registry::list()?;
        return Ok(
            json!({"status":"ok","summary":format!("{} registered repository(ies)",repos.len()),"repos":repos}),
        );
    }
    let root = match args.get("repo_root").and_then(Value::as_str) {
        Some(dir) => super::absolute(Path::new(dir))?,
        None => root.to_owned(),
    };
    let data = Value::Object(args);
    if name == "build_or_update_graph_tool" {
        return with_provenance(
            super::build::run(&root, &data)?,
            &store::database(&root, None),
            &root,
        );
    }
    if name == "get_docs_section_tool" {
        return Ok(docs(
            data["section_name"]
                .as_str()
                .ok_or_else(|| anyhow!("missing required argument: section_name"))?,
            &root,
        ));
    }
    if !matches!(
        name,
        "list_graph_stats_tool"
            | "query_graph_tool"
            | "semantic_search_nodes_tool"
            | "get_wiki_page_tool"
    ) && !reports::NAMES.contains(&name)
    {
        anyhow::bail!("Unknown tool: '{name}'");
    }
    let db = store::database(&root, None);
    if name == "get_wiki_page_tool" {
        let page = wiki::read(
            &db,
            data["community_name"]
                .as_str()
                .ok_or_else(|| anyhow!("community_name must be a string"))?,
            data["max_chars"].as_i64().unwrap_or(20000),
        )?;
        return with_provenance(page, &db, &root);
    }
    let conn = store::open(&db)?;
    store::check_root(&conn, &db, &root)?;
    let result = match name {
        "semantic_search_nodes_tool" => search::run(&conn, &root, &serde_json::from_value(data)?)?,
        "list_graph_stats_tool" => store::tool_stats(&conn, &root)?,
        report if reports::NAMES.contains(&report) => {
            reports::run(&conn, &root, report, &data, hints)?
        }
        _ => query::run(&conn, &root, &serde_json::from_value(data)?)?,
    };
    attach_provenance(result, &conn, &root)
}

fn with_provenance(data: Value, db: &Path, root: &Path) -> Result<Value> {
    match store::open(db) {
        Ok(conn) => attach_provenance(data, &conn, root),
        Err(_) => Ok(data),
    }
}
fn attach_provenance(mut result: Value, conn: &rusqlite::Connection, root: &Path) -> Result<Value> {
    let provenance = store::provenance(conn, root)?;
    if provenance.as_object().is_some_and(|p| !p.is_empty()) {
        result["_graph"] = provenance;
    }
    Ok(result)
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        serde_json::from_str(include_str!("../../assets/cli/server.json"))
            .expect("bundled server information")
    }
    #[allow(
        deprecated,
        reason = "Protocol 2025-11-25 exposes logging/setLevel in the pinned server"
    )]
    async fn set_level(
        &self,
        _: SetLevelRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        Ok(())
    }
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        Err(ErrorData::invalid_params(
            format!("Resource not found: '{}'", request.uri),
            Some(json!({"uri":request.uri})),
        ))
    }
    async fn on_custom_request(
        &self,
        request: CustomRequest,
        _: RequestContext<RoleServer>,
    ) -> Result<CustomResult, ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "Method not found",
            Some(json!(request.method)),
        ))
    }
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.clone()))
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let root = self.root.clone();
        let name = request.name.into_owned();
        let args = request.arguments.unwrap_or_default();
        if let Some(tool) = self.tools.iter().find(|tool| tool.name == name)
            && let Some(required) = tool.input_schema.get("required").and_then(Value::as_array)
        {
            let missing: Vec<_> = required
                .iter()
                .filter_map(Value::as_str)
                .filter(|name| !args.contains_key(*name))
                .collect();
            if !missing.is_empty() {
                let plural = if missing.len() == 1 { "" } else { "s" };
                let mut message = format!(
                    "{} validation error{plural} for call[{name}]",
                    missing.len()
                );
                let input = wire::dict_repr(&args);
                for arg in missing {
                    message += &format!(
                        "\n{arg}\n  Missing required argument [type=missing_argument, input_value={input}, input_type=dict]\n    For further information visit https://errors.pydantic.dev/2.13/v/missing_argument"
                    );
                }
                return Ok(CallToolResult::error(vec![ContentBlock::text(message)]).into());
            }
        }
        let hints = self.hints.clone();
        let result = tokio::task::spawn_blocking(move || call(&root, &name, args, &hints))
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let packet = match result {
            Ok(data) => CallToolResult::structured(data),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(format!("{e:#}"))]),
        };
        Ok(packet.into())
    }
    async fn list_prompts(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(prompts::list().map_err(
            |e| ErrorData::internal_error(e.to_string(), None),
        )?))
    }
    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        prompts::get(&request.name, &request.arguments.unwrap_or_default())
            .map(Into::into)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))
    }
}

pub fn serve(root: &Path) -> Result<()> {
    let server = Server {
        root: root.to_owned(),
        hints: Arc::default(),
        tools: serde_json::from_str(include_str!("../../assets/cli/tools.json"))?,
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            server.serve(stdio()).await?.waiting().await?;
            Ok(())
        })
}

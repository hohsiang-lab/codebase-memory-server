use super::*;
use rmcp::{
    model::{CallToolRequestParams, CallToolResponse, ListToolsResult, PaginatedRequestParams,
        ServerCapabilities, ServerConfig, ProtocolVersion, Implementation},
    service::RequestContext,
    transport::{child_process::TokioChildProcess, streamable_http_server::{
        StreamableHttpService, session::local::LocalSessionManager,
    }},
    ErrorData, Peer, RoleClient, RoleServer, ServerHandler, ServiceExt,
};
use std::sync::Arc;

const TOOLS: &[&str] = &[
    "list_projects", "index_status", "check_index_coverage", "search_graph",
    "trace_path", "detect_changes", "query_graph", "get_graph_schema",
    "compare_graphs", "get_code_snippet", "get_file_outline",
    "get_architecture", "search_code",
];

fn permitted(name: &str, args: Option<&serde_json::Map<String, Value>>, projects: &BTreeSet<String>) -> bool {
    if !TOOLS.contains(&name) { return false }
    if name == "list_projects" { return true }
    let Some(args) = args else { return false };
    let allowed = |key: &str| args.get(key).and_then(Value::as_str).is_some_and(|value| projects.contains(value));
    // Require canonical project IDs, not path/alias/tail resolution. All
    // approved repositories are visible to every authenticated network client.
    if name == "compare_graphs" {
        allowed("base_project") && allowed("target_project")
    } else {
        allowed("project")
    }
}

#[derive(Clone)]
struct Gateway {
    peer: Peer<RoleClient>,
    projects: Arc<BTreeSet<String>>,
}

impl ServerHandler for Gateway {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::from_build_env())
    }

    async fn list_tools(&self, request: Option<PaginatedRequestParams>, _ctx: RequestContext<RoleServer>) -> std::result::Result<ListToolsResult, ErrorData> {
        let mut result = self.peer.list_tools(request).await
            .map_err(|_| ErrorData::internal_error("upstream tool discovery failed", None))?;
        result.tools.retain(|tool| TOOLS.contains(&tool.name.as_ref()));
        Ok(result)
    }

    async fn call_tool(&self, request: CallToolRequestParams, _ctx: RequestContext<RoleServer>) -> std::result::Result<CallToolResponse, ErrorData> {
        if !permitted(&request.name, request.arguments.as_ref(), &self.projects) {
            return Err(ErrorData::invalid_params("tool or project is not allowed", None));
        }
        let result = self.peer.call_tool(request).await
            .map_err(|_| ErrorData::internal_error("upstream tool call failed", None))?;
        Ok(CallToolResponse::Complete(result))
    }
}

pub async fn serve(config: Config) -> Result<()> {
    let engine = index::Engine::new().await?;
    let available = engine.prepare(&config).await?;
    let mut command = engine.command();
    // The upstream native analysis profile is a second independent read-only
    // boundary. Mutation tools are available only to the local index worker.
    command.arg("--tool-profile=analysis");
    let upstream = timeout(Duration::from_secs(60), ().serve(TokioChildProcess::new(command)?)).await??;
    let gateway = Gateway { peer: upstream.peer().clone(), projects: Arc::new(config.names()) };
    let service = StreamableHttpService::new(
        move || Ok(gateway.clone()),
        LocalSessionManager::default().into(),
        Default::default(),
    );
    let router = axum::Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .nest_service("/mcp", service);
    let addr = env::var("LISTEN_ADDR").unwrap_or_else(|_| "127.0.0.1:8000".to_string());
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tokio::select! {
        result = async { axum::serve(listener, router).await } => result?,
        result = engine.run(config, available) => result?,
        _ = upstream.waiting() => bail!("upstream MCP process exited"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denies_write_tools_and_unapproved_projects() {
        let names = BTreeSet::from(["example".to_string()]);
        let args = json!({"project":"example"});
        assert!(permitted("search_code", args.as_object(), &names));
        assert!(permitted("list_projects", None, &names));
        for tool in ["delete_project", "index_repository", "manage_adr", "ingest_traces"] {
            assert!(!permitted(tool, args.as_object(), &names));
        }
        for project in ["removed", "/repos/example", "*"] {
            let args = json!({"project":project});
            assert!(!permitted("search_code", args.as_object(), &names));
        }
        let args = json!({"base_project":"example", "target_project":"removed"});
        assert!(!permitted("compare_graphs", args.as_object(), &names));
    }
}

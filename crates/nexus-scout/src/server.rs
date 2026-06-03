//! Model Context Protocol server (stdio transport).
//!
//! Exposes the same two operations as the CLI (`query_cypher` and `get_schema`)
//! as MCP tools, forwarding both to the shared [`Scout`] core so the MCP and CLI
//! front-ends cannot diverge in validation behavior. Only stdio is wired for the
//! MVP; the handler is transport-agnostic, so SSE is an additive change.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ErrorData, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::{Config, Error, Scout};

/// Parameters for the `query_cypher` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct QueryCypherParams {
    /// A read-only Cypher query.
    cypher: String,
    /// Query parameters bound as `$key` (optional).
    #[serde(default)]
    params: Map<String, Value>,
    /// Row limit override (optional; capped at the configured maximum).
    #[serde(default)]
    limit: Option<u32>,
}

/// The MCP server handler wrapping the shared gateway core.
#[derive(Clone)]
struct ScoutServer {
    scout: Scout,
    tool_router: ToolRouter<Self>,
}

impl ScoutServer {
    fn new(scout: Scout) -> Self {
        Self {
            scout,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl ScoutServer {
    /// Validate and execute a read-only Cypher query against the Pubky graph.
    #[tool(
        description = "Execute a read-only Cypher query against the Pubky social graph (Neo4j). \
                       The query is validated before execution - only MATCH/RETURN-style read \
                       queries are allowed. Call get_schema first to learn the graph structure."
    )]
    async fn query_cypher(
        &self,
        Parameters(QueryCypherParams { cypher, params, limit }): Parameters<QueryCypherParams>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.scout.query(&cypher, params, limit).await {
            Ok(response) => {
                let value =
                    serde_json::to_value(&response).map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(CallToolResult::structured(value))
            }
            // A rejected/timed-out query is a normal tool outcome the agent can
            // react to, so it is returned as an error-flagged result, not a
            // protocol error.
            Err(err) => Ok(CallToolResult::structured_error(error_value(&err))),
        }
    }

    /// Return the Pubky social graph schema (node types, relationships, examples).
    #[tool(description = "Return the Pubky social graph schema: node types with properties, \
                          relationship types with direction and properties, and example queries.")]
    fn get_schema(&self) -> Result<CallToolResult, ErrorData> {
        let value =
            serde_json::to_value(self.scout.schema()).map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::structured(value))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ScoutServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        let mut implementation = Implementation::from_build_env();
        implementation.name = env!("CARGO_PKG_NAME").into();
        implementation.version = env!("CARGO_PKG_VERSION").into();
        info.server_info = implementation;
        info.instructions = Some(
            "Read-only Cypher query gateway for the Pubky social graph. \
             Use get_schema to learn the structure, then query_cypher to run read-only Cypher."
                .to_owned(),
        );
        info
    }
}

fn error_value(err: &Error) -> Value {
    serde_json::to_value(err.to_response()).unwrap_or_else(|_| Value::String(err.to_string()))
}

/// Runs the MCP server over stdio until the client disconnects.
///
/// # Errors
///
/// Returns [`Error`] if the gateway cannot connect or the transport fails.
pub async fn serve_stdio(config: Config) -> Result<(), Error> {
    let scout = Scout::connect(config).await?;
    let service = ScoutServer::new(scout)
        .serve(rmcp::transport::stdio())
        .await
        .map_err(Error::internal)?;
    service.waiting().await.map_err(Error::internal)?;
    Ok(())
}

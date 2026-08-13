mod auth;
mod models;
mod prompts;
mod resources;
mod tools;

use crate::mojang::MojangProfileResolver;
use axum::Router;
use graph_api::models::ApiKey;
use http::request::Parts;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, GetPromptRequestParams, GetPromptResponse,
    ListPromptsResult, ListResourceTemplatesResult, ListToolsResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use sqlx::MySqlPool;

#[derive(Clone, Debug)]
pub(crate) struct Mcp {
    default_pool: MySqlPool,
    punishments_pool: MySqlPool,
    profile_resolver: MojangProfileResolver,
}

impl Mcp {
    fn new(
        default_pool: MySqlPool,
        punishments_pool: MySqlPool,
        profile_resolver: MojangProfileResolver,
    ) -> Self {
        Self {
            default_pool,
            punishments_pool,
            profile_resolver,
        }
    }
}

impl ServerHandler for Mcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_resources()
                .enable_tools()
                .enable_prompts()
                .build(),
        )
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(
            resources::templates(),
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let api_key = self.get_api_key(&context)?;
        self.route_resource(api_key, &request.uri).await
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools::list()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let api_key = self.get_api_key(&context)?;
        self.route_tool(api_key, request).await
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(prompts::list()))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        prompts::get(request)
    }
}

impl Mcp {
    fn get_api_key<'a>(
        &self,
        context: &'a RequestContext<RoleServer>,
    ) -> Result<&'a ApiKey, ErrorData> {
        let parts = context.extensions.get::<Parts>().ok_or_else(|| {
            ErrorData::internal_error("HTTP request context is unavailable", None)
        })?;

        parts
            .extensions
            .get::<ApiKey>()
            .ok_or_else(|| ErrorData::invalid_request("authentication required", None))
    }
}

pub(crate) fn router(
    default_pool: MySqlPool,
    punishments_pool: MySqlPool,
    profile_resolver: MojangProfileResolver,
) -> Router {
    let mcp = Mcp::new(default_pool.clone(), punishments_pool, profile_resolver);

    let service = StreamableHttpService::new(
        move || Ok(mcp.clone()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_allowed_hosts(["graph.azisaba.net"])
            .with_legacy_session_mode(false)
            .with_json_response(true),
    );

    Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn_with_state(
            default_pool,
            auth::authenticate,
        ))
}

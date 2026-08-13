mod auth;
mod resources;

use crate::mojang::MojangProfileResolver;
use axum::Router;
use graph_api::models::ApiKey;
use http::request::Parts;
use rmcp::model::{
    ListResourceTemplatesResult, PaginatedRequestParams, ReadResourceRequestParams,
    ReadResourceResponse, ServerCapabilities, ServerInfo,
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
        ServerInfo::new(ServerCapabilities::builder().enable_resources().build())
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
        let parts = context.extensions.get::<Parts>().ok_or_else(|| {
            ErrorData::internal_error("HTTP request context is unavailable", None)
        })?;

        let api_key = parts
            .extensions
            .get::<ApiKey>()
            .ok_or_else(|| ErrorData::invalid_request("authentication required", None))?;

        self.route_resource(api_key, &request.uri).await
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

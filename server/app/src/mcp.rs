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
    ListResourceTemplatesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler, prompt_handler, tool_handler};
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

#[tool_handler(router = tools::tool_router())]
#[prompt_handler(router = Mcp::prompt_router())]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn all_tools_have_object_input_schema() {
        let router = tools::tool_router();
        let tools = router.list_all();

        assert!(!tools.is_empty(), "No tools registered");

        for tool in tools {
            let schema = &tool.input_schema;
            assert_eq!(
                schema.get("type").and_then(|v| v.as_str()),
                Some("object"),
                "Tool '{}' must have an object root schema",
                tool.name
            );
        }
    }

    #[tokio::test]
    async fn get_network_status_has_valid_empty_object_schema() {
        let router = tools::tool_router();
        let tool = router.get("get_network_status").expect("Tool not found");

        assert_eq!(
            tool.input_schema.get("type").and_then(|v| v.as_str()),
            Some("object")
        );
        let properties = tool.input_schema.get("properties").and_then(|v| v.as_object());
        assert!(properties.is_some(), "Should have properties field");
        assert!(properties.unwrap().is_empty(), "Properties should be empty for parameterless tool");
    }

    #[tokio::test]
    async fn tool_names_are_unique() {
        let router = tools::tool_router();
        let tools = router.list_all();
        let names: std::collections::HashSet<_> = tools.iter().map(|t| &t.name).collect();
        assert_eq!(names.len(), tools.len(), "Tool names are not unique");
    }

    #[tokio::test]
    async fn tool_descriptions_are_non_empty() {
        let router = tools::tool_router();
        let tools = router.list_all();
        for tool in tools {
            assert!(
                tool.description.as_ref().map_or(false, |d| !d.is_empty()),
                "Tool '{}' has an empty description",
                tool.name
            );
        }
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

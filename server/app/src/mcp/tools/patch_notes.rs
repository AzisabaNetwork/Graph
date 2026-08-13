use crate::auth::ApiKeyScopeChecker;
use crate::mcp::Mcp;
use crate::mcp::models::{PatchNoteSummary, ResourceLink};
use chrono::{DateTime, Utc};
use graph_api::models::{ApiKeyScope};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
pub(super) struct SearchPatchNotesArgs {
    /// Text to search in title and body
    pub query: Option<String>,
    /// Inclusive start date
    pub from: Option<DateTime<Utc>>,
    /// Exclusive end date
    pub to: Option<DateTime<Utc>>,
    /// Filter by server/target (e.g., "creativePro", "frontier")
    pub target: Option<String>,
    /// Filter by category (e.g., "balance", "feature", "fix")
    pub category: Option<String>,
    /// Maximum number of results to return (default: 20, max: 100)
    pub limit: Option<u8>,
}

#[tool_router(router = patch_notes_tools, vis = "pub(super)")]
impl Mcp {
    #[tool(
        name = "search_patch_notes",
        description = "Search network patch notes with temporal and category filters."
    )]
    pub(super) async fn search_patch_notes(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<SearchPatchNotesArgs>,
    ) -> Result<Json<Vec<PatchNoteSummary>>, ErrorData> {
        let api_key = self.get_api_key(&ctx)?;
        let args = params.0;
        if !api_key.has_scope(&ApiKeyScope::PatchNotesColonRead) {
            return Err(ErrorData::invalid_request(
                "missing required scope: patch-notes:read",
                None,
            ));
        }

        let limit = args.limit.unwrap_or(20).min(100);

        let mut query = sqlx::QueryBuilder::<sqlx::MySql>::new(
            "SELECT id, title, category, created_at FROM patch_notes WHERE 1=1",
        );

        if let Some(q) = &args.query {
            query.push(" AND (title LIKE ");
            query.push_bind(format!("%{}%", q));
            query.push(" OR body LIKE ");
            query.push_bind(format!("%{}%", q));
            query.push(")");
        }

        if let Some(from) = args.from {
            query.push(" AND created_at >= ");
            query.push_bind(from);
        }

        if let Some(to) = args.to {
            query.push(" AND created_at < ");
            query.push_bind(to);
        }

        if let Some(target) = &args.target {
            query.push(" AND target = ");
            query.push_bind(target);
        }

        if let Some(category) = &args.category {
            query.push(" AND category = ");
            query.push_bind(category);
        }

        query.push(" ORDER BY created_at DESC LIMIT ");
        query.push_bind(limit as i64);

        let rows = query
            .build()
            .fetch_all(&self.default_pool)
            .await
            .map_err(|error| {
                tracing::error!(%error, "failed to search patch notes");
                ErrorData::internal_error("failed to search patch notes", None)
            })?;

        let results: Vec<PatchNoteSummary> = rows
            .into_iter()
            .map(|row| {
                use sqlx::Row;
                PatchNoteSummary {
                    id: row.get("id"),
                    title: row.get("title"),
                    category: row.get("category"),
                    created_at: row.get("created_at"),
                    resource_link: ResourceLink {
                        uri: format!("graph://patch-notes/{}", row.get::<uuid::Uuid, _>("id")),
                    },
                }
            })
            .collect();

        Ok(Json(results))
    }
}

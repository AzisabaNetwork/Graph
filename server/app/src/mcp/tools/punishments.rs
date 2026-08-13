use crate::auth::ApiKeyScopeChecker;
use crate::mcp::Mcp;
use crate::mcp::models::{PunishmentSummary, ResourceLink};
use crate::records::PunishmentRecord;
use graph_api::models::{ApiKeyScope};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
pub(super) struct SearchPunishmentsArgs {
    /// Target identifier (username, UUID, or IP)
    pub target: String,
    /// Maximum number of results to return (default: 20, max: 100)
    pub limit: Option<u8>,
    /// Whether to include inactive punishments
    pub include_inactive: Option<bool>,
}

#[tool_router(router = punishments_tools, vis = "pub(super)")]
impl Mcp {
    #[tool(
        name = "search_punishments",
        description = "Search punishment history for a specific target (username, UUID, or IP)."
    )]
    pub(super) async fn search_punishments(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<SearchPunishmentsArgs>,
    ) -> Result<Json<Vec<PunishmentSummary>>, ErrorData> {
        let api_key = self.get_api_key(&ctx)?;
        let args = params.0;
        if !api_key.has_scope(&ApiKeyScope::PunishmentsColonRead) {
            return Err(ErrorData::invalid_request(
                "missing required scope: punishments:read",
                None,
            ));
        }

        let limit = args.limit.unwrap_or(20).min(100);
        let include_inactive = args.include_inactive.unwrap_or(false);

        let rows = if include_inactive {
            sqlx::query_as::<_, PunishmentRecord>(
                "SELECT * FROM punishmentHistory WHERE target = ? ORDER BY start DESC LIMIT ?",
            )
            .bind(&args.target)
            .bind(limit)
            .fetch_all(&self.punishments_pool)
            .await
        } else {
            sqlx::query_as::<_, PunishmentRecord>(
                "SELECT * FROM punishmentHistory WHERE target = ? AND active = 1 ORDER BY start DESC LIMIT ?",
            )
            .bind(&args.target)
            .bind(limit)
            .fetch_all(&self.punishments_pool)
            .await
        }
        .map_err(|error| {
            tracing::error!(%error, target = %args.target, "failed to search punishments");
            ErrorData::internal_error("failed to search punishments", None)
        })?;

        let punishments: Vec<PunishmentSummary> = rows
            .into_iter()
            .map(|row| PunishmentSummary {
                id: row.id as u64,
                r#type: row.r#type,
                reason: row.reason,
                server: row.server,
                created_at: chrono::DateTime::from_timestamp_millis(row.start)
                    .unwrap_or_else(chrono::Utc::now),
                active: row.active,
                resource_link: ResourceLink {
                    uri: format!("graph://punishments/{}", row.id),
                },
            })
            .collect();

        Ok(Json(punishments))
    }
}

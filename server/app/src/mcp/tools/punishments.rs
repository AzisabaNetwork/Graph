use crate::auth::ApiKeyScopeChecker;
use crate::mcp::Mcp;
use crate::mcp::models::{PunishmentSummary, ResourceLink};
use crate::records::PunishmentRecord;
use graph_api::models::{ApiKey, ApiKeyScope};
use rmcp::ErrorData;
use rmcp::model::{CallToolResult, ContentBlock, TextContent, Tool};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize, JsonSchema)]
pub(super) struct SearchPunishmentsArgs {
    /// Target identifier (username, UUID, or IP)
    pub target: String,
    /// Maximum number of results to return (default: 20, max: 100)
    pub limit: Option<u8>,
    /// Whether to include inactive punishments
    pub include_inactive: Option<bool>,
}

pub(super) fn tools() -> Vec<Tool> {
    let schema = serde_json::to_value(schemars::schema_for!(SearchPunishmentsArgs))
        .unwrap()
        .as_object()
        .unwrap()
        .clone();

    vec![Tool::new(
        "search_punishments",
        "Search punishment history for a specific target (username, UUID, or IP).",
        Arc::new(schema),
    )]
}

impl Mcp {
    pub(super) async fn search_punishments(
        &self,
        api_key: &ApiKey,
        args: SearchPunishmentsArgs,
    ) -> Result<CallToolResult, ErrorData> {
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

        let content = serde_json::to_string(&punishments).map_err(|error| {
            tracing::error!(%error, "failed to serialize punishments");
            ErrorData::internal_error("failed to serialize punishments", None)
        })?;

        Ok(CallToolResult::success(vec![ContentBlock::Text(
            TextContent::new(content),
        )]))
    }
}

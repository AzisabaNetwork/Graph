use crate::auth::ApiKeyScopeChecker;
use crate::mcp::Mcp;
use crate::mcp::models::{PatchNoteSummary, ResourceLink};
use chrono::{DateTime, Utc};
use graph_api::models::{ApiKey, ApiKeyScope};
use rmcp::ErrorData;
use rmcp::model::{CallToolResult, ContentBlock, TextContent, Tool};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

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

pub(super) fn tools() -> Vec<Tool> {
    let schema = serde_json::to_value(schemars::schema_for!(SearchPatchNotesArgs))
        .unwrap()
        .as_object()
        .unwrap()
        .clone();

    vec![Tool::new(
        "search_patch_notes",
        "Search network patch notes with temporal and category filters.",
        Arc::new(schema),
    )]
}

impl Mcp {
    pub(super) async fn search_patch_notes(
        &self,
        api_key: &ApiKey,
        args: SearchPatchNotesArgs,
    ) -> Result<CallToolResult, ErrorData> {
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

        let content = serde_json::to_string(&results).map_err(|error| {
            tracing::error!(%error, "failed to serialize patch notes");
            ErrorData::internal_error("failed to serialize patch notes", None)
        })?;

        Ok(CallToolResult::success(vec![ContentBlock::Text(
            TextContent::new(content),
        )]))
    }
}

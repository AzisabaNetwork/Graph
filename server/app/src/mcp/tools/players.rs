use crate::auth::ApiKeyScopeChecker;
use crate::mcp::Mcp;
use crate::mcp::models::{FriendSummary, PlayerOverview, PlayerRelationships, ResourceLink};
use crate::records::PlayerRecord;
use graph_api::models::{ApiKey, ApiKeyScope};
use rmcp::ErrorData;
use rmcp::model::{CallToolResult, ContentBlock, TextContent, Tool};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Deserialize, JsonSchema)]
pub(super) struct PlayerArgs {
    /// Minecraft username or UUID
    pub player: String,
}

pub(super) fn tools() -> Vec<Tool> {
    let player_schema = serde_json::to_value(schemars::schema_for!(PlayerArgs))
        .unwrap()
        .as_object()
        .unwrap()
        .clone();

    vec![
        Tool::new(
            "get_player_overview",
            "Get a summary of a player's profile, status, and activity counts.",
            Arc::new(player_schema.clone()),
        ),
        Tool::new(
            "get_player_relationships",
            "Get a list of a player's friends and pending friend requests.",
            Arc::new(player_schema),
        ),
    ]
}

impl Mcp {
    pub(super) async fn get_player_overview(
        &self,
        api_key: &ApiKey,
        args: PlayerArgs,
    ) -> Result<CallToolResult, ErrorData> {
        if !api_key.has_any_scope(&[
            ApiKeyScope::PlayersColonRead,
            ApiKeyScope::PlayersColonReadDetails,
        ]) {
            return Err(ErrorData::invalid_request(
                "missing required scope: players:read",
                None,
            ));
        }

        let player_id = self.resolve_player(&args.player).await?;

        let record = sqlx::query_as::<_, PlayerRecord>(
            r#"
            SELECT id, discord_id, bio, status, current_server, current_locale, current_client_version,
                   first_login_at, last_seen_at
            FROM players
            WHERE id = ?
            "#,
        )
        .bind(player_id)
        .fetch_optional(&self.default_pool)
        .await
        .map_err(|error| {
            tracing::error!(%error, %player_id, "failed to fetch player record");
            ErrorData::internal_error("failed to fetch player", None)
        })?
        .unwrap_or_else(|| PlayerRecord::empty(player_id));

        let profile = self
            .profile_resolver
            .find_by_uuid(player_id)
            .await
            .map_err(|error| {
                tracing::error!(%error, %player_id, "failed to resolve player profile");
                ErrorData::internal_error("failed to resolve player profile", None)
            })?
            .ok_or_else(|| ErrorData::invalid_request("player profile not found", None))?;

        let punishment_counts = sqlx::query_as::<_, (i64, i64)>(
            "SELECT COUNT(*), IFNULL(SUM(active), 0) FROM punishmentHistory WHERE target = ?",
        )
        .bind(player_id.to_string())
        .fetch_one(&self.punishments_pool)
        .await
        .unwrap_or((0, 0));

        let friend_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM friendships WHERE player1_id = ? OR player2_id = ?",
        )
        .bind(player_id)
        .bind(player_id)
        .fetch_one(&self.default_pool)
        .await
        .unwrap_or(0);

        let overview = PlayerOverview {
            id: player_id,
            username: profile.username,
            discord_id: if api_key.has_scope(&ApiKeyScope::PlayersColonReadDetails) {
                record.discord_id
            } else {
                None
            },
            status: record.status,
            bio: record.bio,
            first_login_at: record.first_login_at,
            last_seen_at: record.last_seen_at,
            current_server: record.current_server,
            punishment_count: punishment_counts.0 as u64,
            active_punishment_count: punishment_counts.1 as u64,
            friend_count: friend_count as u64,
            resource_link: ResourceLink {
                uri: format!("graph://players/{}", player_id),
            },
        };

        let content = serde_json::to_string(&overview).map_err(|error| {
            tracing::error!(%error, %player_id, "failed to serialize player overview");
            ErrorData::internal_error("failed to serialize player overview", None)
        })?;

        Ok(CallToolResult::success(vec![ContentBlock::Text(
            TextContent::new(content),
        )]))
    }

    pub(super) async fn get_player_relationships(
        &self,
        api_key: &ApiKey,
        args: PlayerArgs,
    ) -> Result<CallToolResult, ErrorData> {
        if !api_key.has_any_scope(&[
            ApiKeyScope::PlayersColonRead,
            ApiKeyScope::PlayersColonReadDetails,
        ]) {
            return Err(ErrorData::invalid_request(
                "missing required scope: players:read",
                None,
            ));
        }

        let player_id = self.resolve_player(&args.player).await?;

        let rows = sqlx::query(
            r#"
            SELECT 
                IF(player1_id = ?, player2_id, player1_id) AS friend_id,
                p.status
            FROM friendships f
            JOIN players p ON p.id = IF(f.player1_id = ?, f.player2_id, f.player1_id)
            WHERE player1_id = ? OR player2_id = ?
            "#,
        )
        .bind(player_id)
        .bind(player_id)
        .bind(player_id)
        .bind(player_id)
        .fetch_all(&self.default_pool)
        .await
        .map_err(|error| {
            tracing::error!(%error, %player_id, "failed to fetch friends");
            ErrorData::internal_error("failed to fetch friends", None)
        })?;

        let mut friend_summaries = Vec::with_capacity(rows.len());
        for row in rows {
            use sqlx::Row;
            let friend_uuid: Uuid = row.get("friend_id");
            let status: String = row.get("status");

            let username = self
                .profile_resolver
                .find_by_uuid(friend_uuid)
                .await
                .ok()
                .flatten()
                .map(|p| p.username)
                .unwrap_or_else(|| "Unknown".to_string());

            friend_summaries.push(FriendSummary {
                id: friend_uuid,
                username,
                status,
            });
        }

        let incoming_requests_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM friend_requests WHERE player_id = ?",
        )
        .bind(player_id)
        .fetch_one(&self.default_pool)
        .await
        .unwrap_or(0);

        let relationships = PlayerRelationships {
            friend_count: friend_summaries.len() as u64,
            friends: friend_summaries,
            incoming_requests_count: incoming_requests_count as u64,
        };

        let content = serde_json::to_string(&relationships).map_err(|error| {
            tracing::error!(%error, %player_id, "failed to serialize player relationships");
            ErrorData::internal_error("failed to serialize player relationships", None)
        })?;

        Ok(CallToolResult::success(vec![ContentBlock::Text(
            TextContent::new(content),
        )]))
    }

    pub(super) async fn resolve_player(&self, identifier: &str) -> Result<Uuid, ErrorData> {
        if let Ok(uuid) = Uuid::parse_str(identifier) {
            return Ok(uuid);
        }

        self.profile_resolver
            .find_by_username(identifier)
            .await
            .map_err(|error| {
                tracing::error!(%error, %identifier, "failed to resolve player username");
                ErrorData::internal_error("failed to resolve player profile", None)
            })?
            .map(|profile| profile.id)
            .ok_or_else(|| {
                ErrorData::invalid_request(format!("player not found: {}", identifier), None)
            })
    }
}

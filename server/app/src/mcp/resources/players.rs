use crate::auth::ApiKeyScopeChecker;
use crate::mcp::Mcp;
use crate::records::PlayerRecord;
use graph_api::models::{ApiKey, ApiKeyScope};
use rmcp::ErrorData;
use rmcp::model::{ReadResourceResponse, ReadResourceResult, ResourceContents, ResourceTemplate};
use uuid::Uuid;

const URI_PREFIX: &str = "graph://players/";

pub(super) fn template() -> ResourceTemplate {
    ResourceTemplate::new(format!("{URI_PREFIX}{{uuid}}"), "player")
}

pub(super) fn parse_uri(uri: &str) -> Option<Uuid> {
    uri.strip_prefix(URI_PREFIX)?.parse().ok()
}

impl Mcp {
    pub(super) async fn read_player_resource(
        &self,
        api_key: &ApiKey,
        player_id: Uuid,
        uri: &str,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if !api_key.has_any_scope(&[
            ApiKeyScope::PlayersColonRead,
            ApiKeyScope::PlayersColonReadDetails,
        ]) {
            return Err(ErrorData::invalid_request(
                "missing required scope: players:read or players:read-etails",
                None,
            ));
        }

        let record = sqlx::query_as::<_, PlayerRecord>(
            r#"
            SELECT id, discord_id, bio, status, current_server, current_locale, current_client_version,
                   first_login_at, last_seen_at
            FROM players
            WHERE id = ?
            "#
        )
        .bind(player_id)
        .fetch_optional(&self.default_pool)
        .await
        .map_err(|error| {
            tracing::error!(%error, %player_id, "failed to fetch player resource");
            ErrorData::internal_error("failed to fetch player", None)
        })?
        .ok_or_else(|| {
            ErrorData::resource_not_found(
                "player not found",
                None,
            )
        })?;

        let profile = self
            .profile_resolver
            .find_by_uuid(player_id)
            .await
            .map_err(|error| {
                tracing::error!(%error, %player_id, "failed to resolve player profile");
                ErrorData::internal_error("failed to resolve player profile", None)
            })?
            .ok_or_else(|| ErrorData::resource_not_found("player profile not found", None))?;

        let player = record.into_player(
            profile.username,
            api_key.has_scope(&ApiKeyScope::PlayersColonReadDetails),
        );

        let content = serde_json::to_string(&player).map_err(|error| {
            tracing::error!(%error, %player_id, "failed to serialize player resource");
            ErrorData::internal_error("failed to serialize player", None)
        })?;

        Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
            vec![ResourceContents::text(content, uri).with_mime_type("application/json")],
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_player_uri() {
        let uuid = Uuid::new_v4();
        let uri = format!("graph://players/{}", uuid);
        assert_eq!(parse_uri(&uri), Some(uuid));
    }

    #[test]
    fn rejects_invalid_player_uri() {
        assert_eq!(parse_uri("graph://players/invalid"), None);
        assert_eq!(parse_uri("graph://other/uuid"), None);
    }
}

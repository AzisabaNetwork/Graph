mod auth;

pub(crate) use auth::authenticate;

use crate::auth::ApiKeyScopeChecker;
use crate::mojang::MojangProfileResolver;
use crate::records::{
    CrawlRecord, PatchNoteRecord, PlayerRecord, ProofRecord, PunishmentProofRecord,
    PunishmentRecord,
};
use chrono::{DateTime, Utc};
use graph_api::models::{ApiKey, ApiKeyScope, Proof, PunishmentType};
use http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ServerCapabilities, ServerInfo};
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{MySql, MySqlPool, QueryBuilder};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub(crate) struct Mcp {
    pub(crate) pool: MySqlPool,
    pub(crate) punishments_pool: MySqlPool,
    pub(crate) mojang: MojangProfileResolver,
}

impl Mcp {
    pub(crate) fn new(
        pool: MySqlPool,
        punishments_pool: MySqlPool,
        mojang: MojangProfileResolver,
    ) -> Self {
        Self {
            pool,
            punishments_pool,
            mojang,
        }
    }

    fn api_key(parts: &Parts) -> Result<&ApiKey, ErrorData> {
        parts.extensions.get::<ApiKey>().ok_or_else(|| {
            ErrorData::internal_error(
                "authenticated API key is missing from request context",
                None,
            )
        })
    }

    fn require_scope(parts: &Parts, scope: ApiKeyScope) -> Result<&ApiKey, ErrorData> {
        let key = Self::api_key(parts)?;
        if key.has_scope(&scope) {
            Ok(key)
        } else {
            Err(ErrorData::invalid_request(
                "API key lacks the required scope",
                None,
            ))
        }
    }

    fn require_player_read(parts: &Parts) -> Result<&ApiKey, ErrorData> {
        let key = Self::api_key(parts)?;
        if key.has_any_scope(&[
            ApiKeyScope::PlayersColonRead,
            ApiKeyScope::PlayersColonReadDetails,
        ]) {
            Ok(key)
        } else {
            Err(ErrorData::invalid_request(
                "API key lacks the required scope",
                None,
            ))
        }
    }

    fn result<T: Serialize>(value: &T) -> Result<CallToolResult, ErrorData> {
        serde_json::to_value(value)
            .map(CallToolResult::structured)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))
    }

    fn not_found(message: impl Into<String>) -> CallToolResult {
        CallToolResult::structured_error(json!({ "error": "not_found", "message": message.into() }))
    }

    fn decode_cursor<T: for<'de> Deserialize<'de>>(
        value: Option<&str>,
    ) -> Result<Option<T>, ErrorData> {
        let Some(value) = value else {
            return Ok(None);
        };
        let bytes =
            base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, value)
                .map_err(|_| ErrorData::invalid_params("cursor is invalid", None))?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| ErrorData::invalid_params("cursor is invalid", None))
    }

    fn encode_cursor<T: Serialize>(value: &T) -> Result<String, ErrorData> {
        let bytes = serde_json::to_vec(value)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        Ok(base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            bytes,
        ))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct PlayerInput {
    #[schemars(description = "Minecraft UUID or username")]
    pub identifier: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlayersInput {
    #[schemars(description = "Maximum number of players to return (1-100)")]
    pub limit: Option<u8>,
    #[schemars(description = "Cursor returned by a previous call")]
    pub cursor: Option<String>,
    pub username: Option<String>,
    #[schemars(description = "Discord user ID; requires players:read-details")]
    pub discord_id: Option<String>,
    pub status: Option<String>,
    pub current_server: Option<String>,
    pub current_locale: Option<String>,
    pub current_client_version: Option<String>,
    pub first_login_from: Option<DateTime<Utc>>,
    pub first_login_to: Option<DateTime<Utc>>,
    pub last_seen_from: Option<DateTime<Utc>>,
    pub last_seen_to: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FriendsInput {
    #[schemars(description = "Minecraft UUID or username")]
    pub identifier: String,
    pub limit: Option<u8>,
    pub cursor: Option<String>,
    #[schemars(description = "Friend UUID filter")]
    pub friend_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FriendRequestsInput {
    #[schemars(description = "Minecraft UUID or username")]
    pub identifier: String,
    pub limit: Option<u8>,
    pub cursor: Option<String>,
    #[schemars(description = "Sender UUID filter")]
    pub sender_id: Option<String>,
}

struct RelationInput {
    identifier: String,
    limit: Option<u8>,
    cursor: Option<String>,
    related_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CrawlInput {
    pub limit: Option<u8>,
    pub cursor: Option<String>,
    pub address: Option<String>,
    pub port: Option<u16>,
    pub version: Option<String>,
    pub protocol_version: Option<i32>,
    pub crawled_from: Option<DateTime<Utc>>,
    pub crawled_to: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PatchNoteInput {
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PatchNotesInput {
    pub limit: Option<u8>,
    pub cursor: Option<String>,
    pub target: Option<String>,
    pub category: Option<String>,
    pub author_id: Option<String>,
    pub created_from: Option<DateTime<Utc>>,
    pub created_to: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PunishmentInput {
    pub id: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PunishmentsInput {
    pub limit: Option<u8>,
    pub cursor: Option<String>,
    pub target: Option<String>,
    #[schemars(description = "Punishment type, such as ban or tempBan")]
    pub r#type: Option<String>,
    pub server: Option<String>,
    pub active: Option<bool>,
    pub created_from: Option<DateTime<Utc>>,
    pub created_to: Option<DateTime<Utc>>,
    pub expires_from: Option<DateTime<Utc>>,
    pub expires_to: Option<DateTime<Utc>>,
    pub revoked_from: Option<DateTime<Utc>>,
    pub revoked_to: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PunishmentProofsInput {
    pub punishment_id: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Page<T> {
    items: Vec<T>,
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct McpCursor<V, T> {
    value: V,
    tie_breaker: T,
}

#[tool_router]
impl Mcp {
    #[tool(
        name = "get_player",
        description = "Get a single Minecraft player by UUID or username. Use this for one specific player; use list_players to search or list multiple players.",
        annotations(read_only_hint = true)
    )]
    async fn get_player(
        &self,
        Parameters(input): Parameters<PlayerInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let key = Self::require_player_read(&parts)?;
        let id = match Uuid::parse_str(&input.identifier) {
            Ok(id) => id,
            Err(_) => match self
                .mojang
                .find_by_username(&input.identifier)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
            {
                Some(profile) => profile.id,
                None => return Ok(Self::not_found("player not found")),
            },
        };
        let Some(profile) = self
            .mojang
            .find_by_uuid(id)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
        else {
            return Ok(Self::not_found("player not found"));
        };
        let record = sqlx::query_as::<_, PlayerRecord>("SELECT id, discord_id, bio, status, current_server, current_locale, current_client_version, first_login_at, last_seen_at FROM players WHERE id = ?")
            .bind(id).fetch_optional(&self.pool).await.map_err(|e| ErrorData::internal_error(e.to_string(), None))?.unwrap_or_else(|| PlayerRecord::empty(id));
        let player = record.into_player(
            profile.username,
            key.has_scope(&ApiKeyScope::PlayersColonReadDetails),
        );
        Self::result(&player)
    }

    #[tool(
        name = "list_players",
        description = "List or filter Minecraft players. Use get_player when asking about one specific player.",
        annotations(read_only_hint = true)
    )]
    async fn list_players(
        &self,
        Parameters(input): Parameters<PlayersInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let key = Self::require_player_read(&parts)?;
        if input.discord_id.is_some() && !key.has_scope(&ApiKeyScope::PlayersColonReadDetails) {
            return Err(ErrorData::invalid_request(
                "players:read-details scope is required for discord_id filtering",
                None,
            ));
        }
        let limit = input.limit.unwrap_or(20).clamp(1, 100) as usize;
        let username_id = if let Some(username) = &input.username {
            self.mojang
                .find_by_username(username)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                .map(|profile| profile.id)
        } else {
            None
        };
        if input.username.is_some() && username_id.is_none() {
            return Self::result(&Page::<graph_api::models::Player> {
                items: Vec::new(),
                next_cursor: None,
            });
        }
        let mut query = QueryBuilder::<MySql>::new(
            "SELECT id, discord_id, bio, status, current_server, current_locale, current_client_version, first_login_at, last_seen_at FROM players WHERE 1=1",
        );
        if let Some(value) = &input.discord_id {
            query.push(" AND discord_id = ").push_bind(value);
        }
        if let Some(value) = &input.status {
            query.push(" AND status = ").push_bind(value);
        }
        if let Some(value) = &input.current_server {
            query.push(" AND current_server = ").push_bind(value);
        }
        if let Some(value) = &input.current_locale {
            query.push(" AND current_locale = ").push_bind(value);
        }
        if let Some(value) = &input.current_client_version {
            query
                .push(" AND current_client_version = ")
                .push_bind(value);
        }
        if let Some(value) = input.first_login_from {
            query.push(" AND first_login_at >= ").push_bind(value);
        }
        if let Some(value) = input.first_login_to {
            query.push(" AND first_login_at < ").push_bind(value);
        }
        if let Some(value) = input.last_seen_from {
            query.push(" AND last_seen_at >= ").push_bind(value);
        }
        if let Some(value) = input.last_seen_to {
            query.push(" AND last_seen_at < ").push_bind(value);
        }
        if let Some(id) = username_id {
            query.push(" AND id = ").push_bind(id);
        }
        if let Some(cursor) = Self::decode_cursor::<McpCursor<Uuid, Uuid>>(input.cursor.as_deref())?
        {
            query.push(" AND id > ").push_bind(cursor.value);
        }
        query
            .push(" ORDER BY id ASC LIMIT ")
            .push_bind((limit + 1) as i64);
        let mut records = query
            .build_query_as::<PlayerRecord>()
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let next_cursor = if records.len() > limit {
            records
                .pop()
                .map(|record| {
                    Self::encode_cursor(&McpCursor {
                        value: record.id,
                        tie_breaker: record.id,
                    })
                })
                .transpose()?
        } else {
            None
        };
        let mut items = Vec::new();
        for record in records {
            let Some(profile) = self
                .mojang
                .find_by_uuid(record.id)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
            else {
                continue;
            };
            items.push(record.into_player(
                profile.username,
                key.has_scope(&ApiKeyScope::PlayersColonReadDetails),
            ));
        }
        Self::result(&Page { items, next_cursor })
    }

    #[tool(
        name = "list_player_friends",
        description = "List a player's friends by UUID or username. Use get_player for one player record.",
        annotations(read_only_hint = true)
    )]
    async fn list_player_friends(
        &self,
        Parameters(input): Parameters<FriendsInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        self.list_relation(
            RelationInput {
                identifier: input.identifier,
                limit: input.limit,
                cursor: input.cursor,
                related_id: input.friend_id,
            },
            parts,
            false,
        )
        .await
    }

    #[tool(
        name = "list_player_friend_requests",
        description = "List incoming friend requests for a player by UUID or username.",
        annotations(read_only_hint = true)
    )]
    async fn list_player_friend_requests(
        &self,
        Parameters(input): Parameters<FriendRequestsInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        self.list_relation(
            RelationInput {
                identifier: input.identifier,
                limit: input.limit,
                cursor: input.cursor,
                related_id: input.sender_id,
            },
            parts,
            true,
        )
        .await
    }

    async fn list_relation(
        &self,
        input: RelationInput,
        parts: Parts,
        requests: bool,
    ) -> Result<CallToolResult, ErrorData> {
        let key = Self::require_player_read(&parts)?;
        let id = self.resolve_player_id(&input.identifier).await?;
        let Some(id) = id else {
            return Ok(Self::not_found("player not found"));
        };
        let limit = input.limit.unwrap_or(20).clamp(1, 100) as usize;
        let cursor = Self::decode_cursor::<McpCursor<Uuid, Uuid>>(input.cursor.as_deref())?;
        let related_id = input
            .related_id
            .as_deref()
            .map(Uuid::parse_str)
            .transpose()
            .map_err(|_| ErrorData::invalid_params("related_id must be a UUID", None))?;
        let ids = if requests {
            let mut query = QueryBuilder::<MySql>::new(
                "SELECT sender_id FROM friend_requests WHERE player_id = ",
            );
            query.push_bind(id);
            if let Some(related_id) = related_id {
                query.push(" AND sender_id = ").push_bind(related_id);
            }
            if let Some(cursor) = &cursor {
                query.push(" AND sender_id > ").push_bind(cursor.value);
            }
            query
                .push(" ORDER BY sender_id ASC LIMIT ")
                .push_bind((limit + 1) as i64);
            query
                .build_query_scalar::<Uuid>()
                .fetch_all(&self.pool)
                .await
        } else {
            let mut query = QueryBuilder::<MySql>::new("SELECT CASE WHEN player1_id = ");
            query.push_bind(id).push(" THEN player2_id ELSE player1_id END AS player_id FROM friendships WHERE (player1_id = ").push_bind(id).push(" OR player2_id = ").push_bind(id).push(")");
            if let Some(related_id) = related_id {
                query
                    .push(" AND ((player1_id = ")
                    .push_bind(id)
                    .push(" AND player2_id = ")
                    .push_bind(related_id)
                    .push(") OR (player2_id = ")
                    .push_bind(id)
                    .push(" AND player1_id = ")
                    .push_bind(related_id)
                    .push("))");
            }
            if let Some(cursor) = &cursor {
                query
                    .push(" AND (CASE WHEN player1_id = ")
                    .push_bind(id)
                    .push(" THEN player2_id ELSE player1_id END) > ")
                    .push_bind(cursor.value);
            }
            query
                .push(" ORDER BY player_id ASC LIMIT ")
                .push_bind((limit + 1) as i64);
            query
                .build_query_scalar::<Uuid>()
                .fetch_all(&self.pool)
                .await
        };
        let mut ids = ids.map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let next_cursor = if ids.len() > limit {
            ids.pop()
                .map(|id| {
                    Self::encode_cursor(&McpCursor {
                        value: id,
                        tie_breaker: id,
                    })
                })
                .transpose()?
        } else {
            None
        };
        let mut items = Vec::new();
        for id in ids {
            if let Some(player) = self.player_value(id, key).await? {
                items.push(player);
            }
        }
        Self::result(&Page { items, next_cursor })
    }

    async fn resolve_player_id(&self, identifier: &str) -> Result<Option<Uuid>, ErrorData> {
        if let Ok(id) = Uuid::parse_str(identifier) {
            return Ok(Some(id));
        }
        Ok(self
            .mojang
            .find_by_username(identifier)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
            .map(|profile| profile.id))
    }

    async fn player_value(
        &self,
        id: Uuid,
        key: &ApiKey,
    ) -> Result<Option<graph_api::models::Player>, ErrorData> {
        let Some(profile) = self
            .mojang
            .find_by_uuid(id)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
        else {
            return Ok(None);
        };
        let record = sqlx::query_as::<_, PlayerRecord>("SELECT id, discord_id, bio, status, current_server, current_locale, current_client_version, first_login_at, last_seen_at FROM players WHERE id = ?").bind(id).fetch_optional(&self.pool).await.map_err(|e| ErrorData::internal_error(e.to_string(), None))?.unwrap_or_else(|| PlayerRecord::empty(id));
        Ok(Some(record.into_player(
            profile.username,
            key.has_scope(&ApiKeyScope::PlayersColonReadDetails),
        )))
    }

    #[tool(
        name = "get_patch_note",
        description = "Get one patch note including its body and image URLs. Use list_patch_notes to search multiple notes.",
        annotations(read_only_hint = true)
    )]
    async fn get_patch_note(
        &self,
        Parameters(input): Parameters<PatchNoteInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        Self::require_scope(&parts, ApiKeyScope::PatchNotesColonRead)?;
        let id = Uuid::parse_str(&input.id)
            .map_err(|_| ErrorData::invalid_params("id must be a UUID", None))?;
        let record = sqlx::query_as::<_, PatchNoteRecord>("SELECT id, target, category, title, body, author_id, created_at FROM patch_notes WHERE id = ?").bind(id).fetch_optional(&self.pool).await.map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let Some(record) = record else {
            return Ok(Self::not_found("patch note not found"));
        };
        let urls = sqlx::query_scalar::<_, String>(
            "SELECT url FROM patch_note_images WHERE patch_note_id = ? ORDER BY position",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Self::result(&record.into_patch_note(urls))
    }

    #[tool(
        name = "list_patch_notes",
        description = "List patch notes with target, category, author, and creation-date filters. Use get_patch_note for one note by ID.",
        annotations(read_only_hint = true)
    )]
    async fn list_patch_notes(
        &self,
        Parameters(input): Parameters<PatchNotesInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        Self::require_scope(&parts, ApiKeyScope::PatchNotesColonRead)?;
        let limit = input.limit.unwrap_or(20).clamp(1, 100) as usize;
        let mut query = QueryBuilder::<MySql>::new(
            "SELECT id, target, category, title, body, author_id, created_at FROM patch_notes WHERE 1=1",
        );
        if let Some(value) = &input.target {
            query.push(" AND target = ").push_bind(value);
        }
        if let Some(value) = &input.category {
            query.push(" AND category = ").push_bind(value);
        }
        if let Some(value) = input.author_id {
            query.push(" AND author_id = ").push_bind(
                Uuid::parse_str(&value)
                    .map_err(|_| ErrorData::invalid_params("author_id must be a UUID", None))?,
            );
        }
        if let Some(value) = input.created_from {
            query.push(" AND created_at >= ").push_bind(value);
        }
        if let Some(value) = input.created_to {
            query.push(" AND created_at < ").push_bind(value);
        }
        if let Some(cursor) =
            Self::decode_cursor::<McpCursor<DateTime<Utc>, Uuid>>(input.cursor.as_deref())?
        {
            query
                .push(" AND (created_at < ")
                .push_bind(cursor.value)
                .push(" OR (created_at = ")
                .push_bind(cursor.value)
                .push(" AND id < ")
                .push_bind(cursor.tie_breaker)
                .push(") )");
        }
        query
            .push(" ORDER BY created_at DESC, id DESC LIMIT ")
            .push_bind((limit + 1) as i64);
        let mut rows = query
            .build_query_as::<PatchNoteRecord>()
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let next_cursor = if rows.len() > limit {
            rows.pop()
                .map(|row| {
                    Self::encode_cursor(&McpCursor {
                        value: row.created_at,
                        tie_breaker: row.id,
                    })
                })
                .transpose()?
        } else {
            None
        };
        let ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
        let mut image_query = QueryBuilder::<MySql>::new(
            "SELECT patch_note_id, url FROM patch_note_images WHERE patch_note_id IN (",
        );
        let mut separated = image_query.separated(", ");
        for id in &ids {
            separated.push_bind(id);
        }
        separated.push_unseparated(") ORDER BY patch_note_id, position");
        let images = image_query
            .build_query_as::<(Uuid, String)>()
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let mut by_id = BTreeMap::<Uuid, Vec<String>>::new();
        for (id, url) in images {
            by_id.entry(id).or_default().push(url);
        }
        let items = rows
            .into_iter()
            .map(|row| {
                let urls = by_id.remove(&row.id).unwrap_or_default();
                row.into_patch_note(urls)
            })
            .collect::<Vec<_>>();
        Self::result(&Page { items, next_cursor })
    }

    #[tool(
        name = "get_network_status",
        description = "Get the latest crawl for each Minecraft server address and port to summarize current network status. Use list_crawls for crawl history.",
        annotations(read_only_hint = true)
    )]
    async fn get_network_status(
        &self,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        Self::require_scope(&parts, ApiKeyScope::CrawlsColonRead)?;
        let rows = sqlx::query_as::<_, CrawlRecord>("SELECT c.id, c.address, c.port, c.ping, c.version, c.protocol_version, c.max_players, c.online_players, c.description, c.favicon, c.crawled_at FROM crawls c WHERE NOT EXISTS (SELECT 1 FROM crawls newer WHERE newer.address = c.address AND newer.port = c.port AND (newer.crawled_at > c.crawled_at OR (newer.crawled_at = c.crawled_at AND newer.id > c.id))) ORDER BY c.address, c.port").fetch_all(&self.pool).await.map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let items = rows
            .into_iter()
            .map(graph_api::models::Crawl::from)
            .collect::<Vec<_>>();
        Self::result(&json!({ "items": items }))
    }

    #[tool(
        name = "list_crawls",
        description = "List crawl history with the supported server and crawl-date filters. Use get_network_status for the latest status per server.",
        annotations(read_only_hint = true)
    )]
    async fn list_crawls(
        &self,
        Parameters(input): Parameters<CrawlInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        Self::require_scope(&parts, ApiKeyScope::CrawlsColonRead)?;
        let limit = input.limit.unwrap_or(20).clamp(1, 100) as usize;
        let mut query = QueryBuilder::<MySql>::new(
            "SELECT id, address, port, ping, version, protocol_version, max_players, online_players, description, favicon, crawled_at FROM crawls WHERE 1=1",
        );
        if let Some(value) = &input.address {
            query.push(" AND address = ").push_bind(value);
        }
        if let Some(value) = input.port {
            query.push(" AND port = ").push_bind(value);
        }
        if let Some(value) = &input.version {
            query.push(" AND version = ").push_bind(value);
        }
        if let Some(value) = input.protocol_version {
            query.push(" AND protocol_version = ").push_bind(value);
        }
        if let Some(value) = input.crawled_from {
            query.push(" AND crawled_at >= ").push_bind(value);
        }
        if let Some(value) = input.crawled_to {
            query.push(" AND crawled_at < ").push_bind(value);
        }
        if let Some(cursor) =
            Self::decode_cursor::<McpCursor<DateTime<Utc>, Uuid>>(input.cursor.as_deref())?
        {
            query
                .push(" AND (crawled_at < ")
                .push_bind(cursor.value)
                .push(" OR (crawled_at = ")
                .push_bind(cursor.value)
                .push(" AND id < ")
                .push_bind(cursor.tie_breaker)
                .push(") )");
        }
        query
            .push(" ORDER BY crawled_at DESC, id DESC LIMIT ")
            .push_bind((limit + 1) as i64);
        let mut rows = query
            .build_query_as::<CrawlRecord>()
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let next_cursor = if rows.len() > limit {
            rows.pop()
                .map(|row| {
                    Self::encode_cursor(&McpCursor {
                        value: row.crawled_at,
                        tie_breaker: row.id,
                    })
                })
                .transpose()?
        } else {
            None
        };
        let items = rows
            .into_iter()
            .map(graph_api::models::Crawl::from)
            .collect::<Vec<_>>();
        Self::result(&Page { items, next_cursor })
    }

    #[tool(
        name = "get_punishment",
        description = "Get one punishment with its proofs and revocation details. Use list_punishments to search punishment history.",
        annotations(read_only_hint = true)
    )]
    async fn get_punishment(
        &self,
        Parameters(input): Parameters<PunishmentInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        Self::require_scope(&parts, ApiKeyScope::PunishmentsColonRead)?;
        let id = i64::try_from(input.id)
            .map_err(|_| ErrorData::invalid_params("id is too large", None))?;
        let row = sqlx::query_as::<_, PunishmentRecord>("SELECT h.id, h.name, h.target, h.reason, h.operator, h.type, h.start, h.end, h.server, h.extra, (p.id IS NOT NULL) AS active, u.id AS revocation_id, u.reason AS revocation_reason, u.timestamp AS revocation_timestamp, u.operator AS revocation_operator FROM punishmentHistory h LEFT JOIN punishments p ON p.id = h.id LEFT JOIN unpunish u ON u.punish_id = h.id WHERE h.id = ? ORDER BY u.id DESC LIMIT 1").bind(id).fetch_optional(&self.punishments_pool).await.map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let Some(row) = row else {
            return Ok(Self::not_found("punishment not found"));
        };
        let proofs = sqlx::query_as::<_, ProofRecord>(
            "SELECT id, text, public FROM proofs WHERE punish_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(&self.punishments_pool)
        .await
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
        .into_iter()
        .map(Proof::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ErrorData::internal_error(e, None))?;
        Self::result(
            &row.into_punishment(proofs)
                .map_err(|e| ErrorData::internal_error(e, None))?,
        )
    }

    #[tool(
        name = "list_punishments",
        description = "List punishment history with target, type, server, active, and date filters. Use get_punishment for one punishment with full details.",
        annotations(read_only_hint = true)
    )]
    async fn list_punishments(
        &self,
        Parameters(input): Parameters<PunishmentsInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        Self::require_scope(&parts, ApiKeyScope::PunishmentsColonRead)?;
        let limit = input.limit.unwrap_or(20).clamp(1, 100) as usize;
        let mut query = QueryBuilder::<MySql>::new(
            "SELECT h.id, h.name, h.target, h.reason, h.operator, h.type, h.start, h.end, h.server, h.extra, (p.id IS NOT NULL) AS active, u.id AS revocation_id, u.reason AS revocation_reason, u.timestamp AS revocation_timestamp, u.operator AS revocation_operator FROM punishmentHistory h LEFT JOIN punishments p ON p.id = h.id LEFT JOIN unpunish u ON u.punish_id = h.id WHERE 1=1",
        );
        if let Some(value) = &input.target {
            query.push(" AND h.target = ").push_bind(value);
        }
        if let Some(value) = input.r#type {
            let kind = value.parse::<PunishmentType>().map_err(|_| {
                ErrorData::invalid_params("type is not a valid punishment type", None)
            })?;
            query
                .push(" AND h.type = ")
                .push_bind(db_punishment_type(kind));
        }
        if let Some(value) = &input.server {
            query
                .push(" AND h.server = ")
                .push_bind(value.to_lowercase());
        }
        if let Some(value) = input.active {
            query.push(if value {
                " AND p.id IS NOT NULL"
            } else {
                " AND p.id IS NULL"
            });
        }
        if let Some(value) = input.created_from {
            query
                .push(" AND h.start >= ")
                .push_bind(value.timestamp_millis());
        }
        if let Some(value) = input.created_to {
            query
                .push(" AND h.start < ")
                .push_bind(value.timestamp_millis());
        }
        if let Some(value) = input.expires_from {
            query
                .push(" AND h.end >= ")
                .push_bind(value.timestamp_millis());
        }
        if let Some(value) = input.expires_to {
            query
                .push(" AND h.end < ")
                .push_bind(value.timestamp_millis());
        }
        if let Some(value) = input.revoked_from {
            query
                .push(" AND u.timestamp >= ")
                .push_bind(value.timestamp_millis());
        }
        if let Some(value) = input.revoked_to {
            query
                .push(" AND u.timestamp < ")
                .push_bind(value.timestamp_millis());
        }
        if let Some(cursor) = Self::decode_cursor::<McpCursor<i64, u64>>(input.cursor.as_deref())? {
            let id = i64::try_from(cursor.tie_breaker)
                .map_err(|_| ErrorData::invalid_params("cursor is invalid", None))?;
            query
                .push(" AND (h.start < ")
                .push_bind(cursor.value)
                .push(" OR (h.start = ")
                .push_bind(cursor.value)
                .push(" AND h.id < ")
                .push_bind(id)
                .push("))");
        }
        query
            .push(" ORDER BY h.start DESC, h.id DESC LIMIT ")
            .push_bind((limit + 1) as i64);
        let mut rows = query
            .build_query_as::<PunishmentRecord>()
            .fetch_all(&self.punishments_pool)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let next_cursor = if rows.len() > limit {
            rows.pop()
                .map(|row| {
                    Self::encode_cursor(&McpCursor {
                        value: row.start,
                        tie_breaker: row.id as u64,
                    })
                })
                .transpose()?
        } else {
            None
        };
        let ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
        let proofs = if ids.is_empty() {
            Vec::new()
        } else {
            let mut proof_query = QueryBuilder::<MySql>::new(
                "SELECT punish_id, id, text, public FROM proofs WHERE punish_id IN (",
            );
            let mut separated = proof_query.separated(", ");
            for id in &ids {
                separated.push_bind(id);
            }
            separated.push_unseparated(") ORDER BY punish_id, id");
            proof_query
                .build_query_as::<PunishmentProofRecord>()
                .fetch_all(&self.punishments_pool)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
        };
        let mut by_id = BTreeMap::<i64, Vec<_>>::new();
        for proof in proofs {
            by_id.entry(proof.punish_id).or_default().push(
                proof
                    .into_proof()
                    .map_err(|e| ErrorData::internal_error(e, None))?,
            );
        }
        let items = rows
            .into_iter()
            .map(|row| {
                let p = by_id.remove(&row.id).unwrap_or_default();
                row.into_punishment(p)
                    .map_err(|e| ErrorData::internal_error(e, None))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::result(&Page { items, next_cursor })
    }

    #[tool(
        name = "list_punishment_proofs",
        description = "List the proofs attached to one punishment. Use this when the caller specifically wants to inspect evidence.",
        annotations(read_only_hint = true)
    )]
    async fn list_punishment_proofs(
        &self,
        Parameters(input): Parameters<PunishmentProofsInput>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        Self::require_scope(&parts, ApiKeyScope::PunishmentsColonRead)?;
        let id = i64::try_from(input.punishment_id)
            .map_err(|_| ErrorData::invalid_params("punishment_id is too large", None))?;
        let exists = sqlx::query_scalar::<_, i64>("SELECT id FROM punishmentHistory WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.punishments_pool)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        if exists.is_none() {
            return Ok(Self::not_found("punishment not found"));
        }
        let proofs = sqlx::query_as::<_, ProofRecord>(
            "SELECT id, text, public FROM proofs WHERE punish_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(&self.punishments_pool)
        .await
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
        .into_iter()
        .map(Proof::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ErrorData::internal_error(e, None))?;
        Self::result(&json!({ "items": proofs }))
    }
}

fn db_punishment_type(value: PunishmentType) -> &'static str {
    match value {
        PunishmentType::Ban => "BAN",
        PunishmentType::TempBan => "TEMP_BAN",
        PunishmentType::IpBan => "IP_BAN",
        PunishmentType::TempIpBan => "TEMP_IP_BAN",
        PunishmentType::Mute => "MUTE",
        PunishmentType::TempMute => "TEMP_MUTE",
        PunishmentType::IpMute => "IP_MUTE",
        PunishmentType::TempIpMute => "TEMP_IP_MUTE",
        PunishmentType::Warning => "WARNING",
        PunishmentType::Caution => "CAUTION",
        PunishmentType::Kick => "KICK",
        PunishmentType::Note => "NOTE",
    }
}

#[tool_handler]
impl ServerHandler for Mcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}

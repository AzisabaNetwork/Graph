use crate::api::Api;
use crate::auth::context::current_api_key;
use crate::auth::scope::ApiKeyScopeExt;
use crate::mojang::{MojangClient, MojangError};
use crate::pagination::Cursor;
use async_trait::async_trait;
use axum::extract::Host;
use axum_extra::extract::CookieJar;
use graph_api::apis::players::{
    GetPlayerByIdResponse, ListPlayersResponse, Players, UpdatePlayerByIdResponse,
};
use graph_api::models::{
    ApiKey, ApiKeyScope, GetPlayerByIdPathParams, ListPlayers200Response,
    ListPlayers200ResponseItemsInner, ListPlayersQueryParams, PlayerStatus,
    UpdatePlayerByIdPathParams, UpdatePlayerByIdRequest,
};
use graph_api::types::Nullable;
use http::Method;
use sqlx::{FromRow, MySql, QueryBuilder};
use std::str::FromStr;
use tokio::task::JoinSet;
use uuid::Uuid;

const DEFAULT_PLAYERS_LIMIT: i32 = 20;
const MAX_PLAYERS_LIMIT: i32 = 100;

type PlayerCursor = Cursor<Uuid, Uuid>;

#[derive(Debug, FromRow)]
struct PlayerRecord {
    id: Uuid,
    discord_id: Option<String>,
    status: String,
    current_server: Option<String>,
    bio: Option<String>,
}

#[async_trait]
impl Players for Api {
    async fn get_player_by_id(
        &self,
        _method: Method,
        _host: Host,
        _cookies: CookieJar,
        path_params: GetPlayerByIdPathParams,
    ) -> Result<GetPlayerByIdResponse, String> {
        let api_key = current_api_key()?;
        if !api_key.has_scope(&ApiKeyScope::PlayersColonRead) {
            return Ok(
                GetPlayerByIdResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }

        let record = fetch_player(&self.pool, path_params.player_id).await?;
        let Some(record) = record else {
            return Ok(GetPlayerByIdResponse::Status404_ThePlayerWasNotFound);
        };
        let Some(username) = resolve_username(&self.mojang, record.id).await? else {
            return Ok(GetPlayerByIdResponse::Status404_ThePlayerWasNotFound);
        };

        Ok(
            GetPlayerByIdResponse::Status200_ThePlayerWasRetrievedSuccessfully(player_from_record(
                record,
                username,
                can_read_discord_id(&api_key),
            )),
        )
    }

    async fn list_players(
        &self,
        _method: Method,
        _host: Host,
        _cookies: CookieJar,
        query_params: ListPlayersQueryParams,
    ) -> Result<ListPlayersResponse, String> {
        let api_key = current_api_key()?;
        if !api_key.has_scope(&ApiKeyScope::PlayersColonRead) {
            return Ok(ListPlayersResponse::Status403_TheAPIKeyLacksTheRequiredScope);
        }

        let can_read_discord_id = can_read_discord_id(&api_key);
        if query_params.discord_id.is_some() && !can_read_discord_id {
            return Ok(ListPlayersResponse::Status403_TheAPIKeyLacksTheRequiredScope);
        }

        let limit = query_params.limit.unwrap_or(DEFAULT_PLAYERS_LIMIT);
        if !(1..=MAX_PLAYERS_LIMIT).contains(&limit) {
            return Ok(ListPlayersResponse::Status400_TheRequestContainsInvalidQueryParameters);
        }
        let limit = limit as usize;

        let cursor = match query_params.cursor.as_deref() {
            Some(cursor) => match PlayerCursor::decode(cursor) {
                Ok(cursor) if cursor.value == cursor.tie_breaker => Some(cursor),
                Err(_) => {
                    return Ok(
                        ListPlayersResponse::Status400_TheRequestContainsInvalidQueryParameters,
                    );
                }
                Ok(_) => {
                    return Ok(
                        ListPlayersResponse::Status400_TheRequestContainsInvalidQueryParameters,
                    );
                }
            },
            None => None,
        };
        let status = match query_params.status.as_deref() {
            Some(status) => match PlayerStatus::from_str(status) {
                Ok(status) => Some(status.to_string()),
                Err(_) => {
                    return Ok(
                        ListPlayersResponse::Status400_TheRequestContainsInvalidQueryParameters,
                    );
                }
            },
            None => None,
        };

        let mut query = QueryBuilder::<MySql>::new(
            "SELECT id, discord_id, status, current_server, bio \
             FROM players WHERE 1 = 1",
        );
        if let Some(discord_id) = query_params.discord_id {
            query.push(" AND discord_id = ").push_bind(discord_id);
        }
        if let Some(status) = status {
            query.push(" AND status = ").push_bind(status);
        }
        if let Some(cursor) = cursor {
            query.push(" AND id > ").push_bind(cursor.value);
        }
        query
            .push(" ORDER BY id ASC LIMIT ")
            .push_bind((limit + 1) as i64);

        let mut rows = query
            .build_query_as::<PlayerRecord>()
            .fetch_all(&self.pool)
            .await
            .map_err(log_database_error)?;

        let next_cursor = if rows.len() > limit {
            rows.pop();
            let last = rows.last().expect("page must include at least one row");
            Some(
                PlayerCursor {
                    value: last.id,
                    tie_breaker: last.id,
                }
                .encode()
                .map_err(|error| {
                    tracing::error!(?error, "failed to encode players cursor");
                    "failed to encode players cursor".to_string()
                })?,
            )
        } else {
            None
        };

        let items = resolve_players(&self.mojang, rows, can_read_discord_id).await?;

        Ok(
            ListPlayersResponse::Status200_ThePlayersWereRetrievedSuccessfully(
                ListPlayers200Response::new(items, into_nullable(next_cursor)),
            ),
        )
    }

    async fn update_player_by_id(
        &self,
        _method: Method,
        _host: Host,
        _cookies: CookieJar,
        path_params: UpdatePlayerByIdPathParams,
        body: UpdatePlayerByIdRequest,
    ) -> Result<UpdatePlayerByIdResponse, String> {
        let api_key = current_api_key()?;
        if !api_key.has_scope(&ApiKeyScope::PlayersColonWrite) {
            return Ok(
                UpdatePlayerByIdResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }

        if body.discord_id.is_none()
            && body.status.is_none()
            && body.current_server.is_none()
            && body.bio.is_none()
        {
            return Ok(UpdatePlayerByIdResponse::Status400_TheRequestBodyIsInvalid);
        }
        let status = match body.status {
            Some(status) => match PlayerStatus::from_str(&status) {
                Ok(status) => Some(status.to_string()),
                Err(_) => {
                    return Ok(UpdatePlayerByIdResponse::Status400_TheRequestBodyIsInvalid);
                }
            },
            None => None,
        };
        if body
            .bio
            .as_ref()
            .is_some_and(|bio| !(1..=160).contains(&bio.chars().count()))
        {
            return Ok(UpdatePlayerByIdResponse::Status400_TheRequestBodyIsInvalid);
        }

        /* let Some(username) = resolve_username(&self.mojang, path_params.player_id).await? else {
            return Ok(
                UpdatePlayerByIdResponse::Status404_NoMinecraftUserExistsWithTheSpecifiedPlayerID,
            );
        }; */
        let username = "now not available".to_string();

        let mut transaction = self.pool.begin().await.map_err(log_database_error)?;
        sqlx::query("INSERT IGNORE INTO players (id) VALUES (?)")
            .bind(path_params.player_id)
            .execute(&mut *transaction)
            .await
            .map_err(log_database_error)?;

        let mut query = QueryBuilder::<MySql>::new("UPDATE players SET ");
        let mut updates = query.separated(", ");
        if let Some(discord_id) = body.discord_id {
            updates
                .push("discord_id = ")
                .push_bind_unseparated(discord_id);
        }
        if let Some(status) = status {
            updates.push("status = ").push_bind_unseparated(status);
        }
        if let Some(current_server) = body.current_server {
            updates
                .push("current_server = ")
                .push_bind_unseparated(current_server);
        }
        if let Some(bio) = body.bio {
            updates.push("bio = ").push_bind_unseparated(bio);
        }
        query.push(" WHERE id = ").push_bind(path_params.player_id);
        query
            .build()
            .execute(&mut *transaction)
            .await
            .map_err(log_database_error)?;

        let record = sqlx::query_as::<_, PlayerRecord>(
            r#"
            SELECT id, discord_id, status, current_server, bio
            FROM players
            WHERE id = ?
            "#,
        )
        .bind(path_params.player_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(log_database_error)?;
        transaction.commit().await.map_err(log_database_error)?;

        Ok(
            UpdatePlayerByIdResponse::Status200_ThePlayerWasUpdatedSuccessfully(
                player_from_record(record, username, can_read_discord_id(&api_key)),
            ),
        )
    }
}

async fn fetch_player(
    pool: &sqlx::MySqlPool,
    player_id: Uuid,
) -> Result<Option<PlayerRecord>, String> {
    sqlx::query_as::<_, PlayerRecord>(
        r#"
        SELECT id, discord_id, status, current_server, bio
        FROM players
        WHERE id = ?
        "#,
    )
    .bind(player_id)
    .fetch_optional(pool)
    .await
    .map_err(log_database_error)
}

fn can_read_discord_id(api_key: &ApiKey) -> bool {
    api_key.has_scope(&ApiKeyScope::PlayersColonReadDetails)
}

fn player_from_record(
    record: PlayerRecord,
    username: String,
    can_read_discord_id: bool,
) -> ListPlayers200ResponseItemsInner {
    let PlayerRecord {
        id,
        discord_id,
        status,
        current_server,
        bio,
    } = record;

    ListPlayers200ResponseItemsInner::new(
        id,
        into_nullable(can_read_discord_id.then_some(discord_id).flatten()),
        username,
        status,
        into_nullable(current_server),
        into_nullable(bio),
    )
}

async fn resolve_players(
    mojang: &MojangClient,
    records: Vec<PlayerRecord>,
    can_read_discord_id: bool,
) -> Result<Vec<ListPlayers200ResponseItemsInner>, String> {
    let mut tasks = JoinSet::new();
    let mut players = std::iter::repeat_with(|| None)
        .take(records.len())
        .collect::<Vec<_>>();

    for (index, record) in records.into_iter().enumerate() {
        let mojang = mojang.clone();
        tasks.spawn(async move {
            let username = mojang.username(record.id).await?;
            Ok::<_, MojangError>((index, record, username))
        });
    }

    while let Some(result) = tasks.join_next().await {
        let (index, record, username) = result
            .map_err(|error| format!("Mojang profile lookup task failed: {error}"))?
            .map_err(log_mojang_error)?;
        let username = username.ok_or_else(|| {
            format!(
                "Mojang profile was not found for tracked player {}",
                record.id
            )
        })?;
        players[index] = Some(player_from_record(record, username, can_read_discord_id));
    }

    Ok(players
        .into_iter()
        .map(|player| player.expect("every Mojang lookup task must produce a player"))
        .collect())
}

async fn resolve_username(
    mojang: &MojangClient,
    player_id: Uuid,
) -> Result<Option<String>, String> {
    mojang.username(player_id).await.map_err(log_mojang_error)
}

fn into_nullable<T>(value: Option<T>) -> Nullable<T> {
    value.map_or(Nullable::Null, Nullable::Present)
}

fn log_database_error(error: sqlx::Error) -> String {
    tracing::error!(?error, "player database operation failed");
    error.to_string()
}

fn log_mojang_error(error: MojangError) -> String {
    tracing::error!(%error, "Mojang profile lookup failed");
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_conversion_hides_discord_id_without_details_scope() {
        let player = player_from_record(player_record(), "Steve".to_string(), false);

        assert_eq!(player.discord_id, Nullable::Null);
        assert_eq!(
            player.current_server,
            Nullable::Present("lobby".to_string())
        );
    }

    #[test]
    fn player_conversion_includes_discord_id_with_details_scope() {
        let player = player_from_record(player_record(), "Steve".to_string(), true);

        assert_eq!(
            player.discord_id,
            Nullable::Present("123456789012345678".to_string())
        );
    }

    fn player_record() -> PlayerRecord {
        PlayerRecord {
            id: Uuid::nil(),
            discord_id: Some("123456789012345678".to_string()),
            status: PlayerStatus::Online.to_string(),
            current_server: Some("lobby".to_string()),
            bio: None,
        }
    }
}

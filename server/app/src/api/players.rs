use crate::api::stream::{
    friend_request_accepted_event, friend_request_added_event, friend_request_rejected_event,
    friend_request_removed_event,
};
use crate::api::{Api, from_nullable, into_nullable};
use crate::auth::scope::ApiKeyScopeExt;
use crate::mojang::PlayerDbError;
use crate::pagination::Cursor;
use async_trait::async_trait;
use axum_extra::extract::CookieJar;
use graph_api::apis::players::{
    AcceptPlayerFriendRequestResponse, AddPlayerFriendRequestResponse, AddPlayerFriendResponse,
    GetPlayerByIdResponse, ListPlayerFriendRequestsResponse, ListPlayerFriendsResponse,
    ListPlayersResponse, Players, RejectPlayerFriendRequestResponse,
    RemovePlayerFriendRequestResponse, RemovePlayerFriendResponse, UpdatePlayerByIdResponse,
};
use graph_api::models::{
    AcceptPlayerFriendRequestPathParams, AddPlayerFriendPathParams,
    AddPlayerFriendRequestPathParams, ApiKey, ApiKeyScope, GetPlayerByIdPathParams,
    ListPlayerFriendRequestsPathParams, ListPlayerFriendRequestsQueryParams,
    ListPlayerFriendsPathParams, ListPlayerFriendsQueryParams, ListPlayers200Response,
    ListPlayersQueryParams, Player, PlayerStatus, RejectPlayerFriendRequestPathParams,
    RemovePlayerFriendPathParams, RemovePlayerFriendRequestPathParams, UpdatePlayerByIdPathParams,
    UpdatePlayerByIdRequest,
};
use headers::Host;
use http::Method;
use sqlx::{FromRow, MySql, QueryBuilder};
use std::{collections::HashMap, str::FromStr};
use tokio::task::JoinSet;
use uuid::Uuid;

const DEFAULT_PLAYERS_LIMIT: u8 = 20;
const MAX_PLAYERS_LIMIT: u8 = 100;

type PlayerCursor = Cursor<Uuid, Uuid>;

#[derive(Debug, FromRow)]
struct PlayerRecord {
    id: Uuid,
    discord_id: Option<String>,
    status: String,
    current_server: Option<String>,
    bio: Option<String>,
}

impl PlayerRecord {
    fn empty(id: Uuid) -> Self {
        Self {
            id,
            discord_id: None,
            status: PlayerStatus::Offline.to_string(),
            current_server: None,
            bio: None,
        }
    }

    fn into_list_item(self, username: String, include_details: bool) -> Player {
        let PlayerRecord {
            id,
            discord_id,
            status,
            current_server,
            bio,
        } = self;

        Player::new(
            id,
            into_nullable(include_details.then_some(discord_id).flatten()),
            username,
            status,
            into_nullable(current_server),
            into_nullable(bio),
        )
    }
}

impl Api {
    async fn load_player_record(&self, id: Uuid) -> Result<PlayerRecord, String> {
        Ok(sqlx::query_as::<_, PlayerRecord>(
            r#"
            SELECT id, discord_id, status, current_server, bio
            FROM players
            WHERE id = ?
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_database_error)?
        .unwrap_or_else(|| PlayerRecord::empty(id)))
    }

    async fn load_player(&self, id: Uuid, include_details: bool) -> Result<Option<Player>, String> {
        let Some(username) = self.player_db.get_username_by_uuid(id).await? else {
            return Ok(None);
        };
        let record = self.load_player_record(id).await?;
        Ok(Some(record.into_list_item(username, include_details)))
    }

    async fn load_players(
        &self,
        ids: &[Uuid],
        include_details: bool,
    ) -> Result<Vec<Player>, String> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut query = QueryBuilder::<MySql>::new(
            "SELECT id, discord_id, status, current_server, bio FROM players WHERE id IN (",
        );
        let mut separated = query.separated(", ");
        for id in ids {
            separated.push_bind(id);
        }
        separated.push_unseparated(")");

        let records = query
            .build_query_as::<PlayerRecord>()
            .fetch_all(&self.pool)
            .await
            .map_err(log_database_error)?;
        let mut records = records
            .into_iter()
            .map(|record| (record.id, record))
            .collect::<HashMap<_, _>>();

        let mut tasks = JoinSet::new();
        for (index, id) in ids.iter().copied().enumerate() {
            let player_db = self.player_db.clone();
            let record = records
                .remove(&id)
                .unwrap_or_else(|| PlayerRecord::empty(id));
            tasks.spawn(async move {
                let username = player_db.get_username_by_uuid(id).await?;
                Ok::<_, PlayerDbError>((index, record, username))
            });
        }

        let mut items = std::iter::repeat_with(|| None)
            .take(ids.len())
            .collect::<Vec<_>>();
        while let Some(result) = tasks.join_next().await {
            let (index, record, username) = result
                .map_err(|error| format!("PlayerDB lookup task failed: {error}"))?
                .map_err(|error| {
                    tracing::error!(%error, "PlayerDB profile lookup failed");
                    error.to_string()
                })?;
            if let Some(username) = username {
                items[index] = Some(record.into_list_item(username, include_details));
            }
        }

        Ok(items.into_iter().flatten().collect())
    }

    async fn players_exist(&self, first: Uuid, second: Uuid) -> Result<bool, String> {
        let (first, second) = tokio::try_join!(
            self.player_db.get_username_by_uuid(first),
            self.player_db.get_username_by_uuid(second),
        )?;
        Ok(first.is_some() && second.is_some())
    }

    async fn load_friend_request_players(
        &self,
        receiver_id: Uuid,
        sender_id: Uuid,
    ) -> Result<Option<(Player, Player)>, String> {
        let (receiver, sender) = tokio::try_join!(
            self.load_player(receiver_id, false),
            self.load_player(sender_id, false),
        )?;
        Ok(match (sender, receiver) {
            (Some(sender), Some(receiver)) => Some((sender, receiver)),
            _ => None,
        })
    }
}

#[async_trait]
impl Players<String> for Api {
    type Claims = ApiKey;

    async fn accept_player_friend_request(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        path_params: &AcceptPlayerFriendRequestPathParams,
    ) -> Result<AcceptPlayerFriendRequestResponse, String> {
        if !api_key.has_scope(&ApiKeyScope::PlayersColonWrite) {
            return Ok(
                AcceptPlayerFriendRequestResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }
        let Some((sender, receiver)) = self
            .load_friend_request_players(path_params.player_id, path_params.sender_id)
            .await?
        else {
            return Ok(AcceptPlayerFriendRequestResponse::Status404_ThePlayer);
        };

        let mut transaction = self.pool.begin().await.map_err(log_database_error)?;
        let request =
            sqlx::query("DELETE FROM friend_requests WHERE player_id = ? AND sender_id = ?")
                .bind(path_params.player_id)
                .bind(path_params.sender_id)
                .execute(&mut *transaction)
                .await
                .map_err(log_database_error)?;
        if request.rows_affected() == 0 {
            transaction.rollback().await.map_err(log_database_error)?;
            return Ok(AcceptPlayerFriendRequestResponse::Status404_ThePlayer);
        }

        let (player1_id, player2_id) =
            normalize_friendship(path_params.player_id, path_params.sender_id);
        match sqlx::query(
            r#"
            INSERT INTO friendships (player1_id, player2_id)
            VALUES (?, ?)
            "#,
        )
        .bind(player1_id)
        .bind(player2_id)
        .execute(&mut *transaction)
        .await
        {
            Ok(_) => {}
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                transaction.rollback().await.map_err(log_database_error)?;
                return Ok(
                    AcceptPlayerFriendRequestResponse::Status409_ThePlayerIsAlreadyFriendsWithTheSender,
                );
            }
            Err(error) => return Err(log_database_error(error)),
        }

        transaction.commit().await.map_err(log_database_error)?;
        self.publish_stream_event(friend_request_accepted_event(sender, receiver))
            .await;
        Ok(AcceptPlayerFriendRequestResponse::Status204_TheFriendRequestWasAcceptedSuccessfully)
    }

    async fn add_player_friend(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        path_params: &AddPlayerFriendPathParams,
    ) -> Result<AddPlayerFriendResponse, String> {
        if !api_key.has_scope(&ApiKeyScope::PlayersColonWrite) {
            return Ok(
                AddPlayerFriendResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }

        if path_params.player_id == path_params.friend_id {
            return Ok(AddPlayerFriendResponse::Status400_TheRequestIsInvalid);
        }

        if !self
            .players_exist(path_params.player_id, path_params.friend_id)
            .await?
        {
            return Ok(AddPlayerFriendResponse::Status404_ThePlayerOrFriendWasNotFound);
        }

        let (player1_id, player2_id) =
            normalize_friendship(path_params.player_id, path_params.friend_id);

        let mut transaction = self.pool.begin().await.map_err(log_database_error)?;
        match sqlx::query(
            r#"
            INSERT INTO friendships (player1_id, player2_id)
            VALUES (?, ?)
            "#,
        )
        .bind(player1_id)
        .bind(player2_id)
        .execute(&mut *transaction)
        .await
        {
            Ok(_) => {}
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => return Ok(
                AddPlayerFriendResponse::Status409_ThePlayerIsAlreadyFriendsWithTheSpecifiedPlayer,
            ),
            Err(error) => return Err(log_database_error(error)),
        }

        sqlx::query(
            r#"
            DELETE FROM friend_requests
            WHERE (player_id = ? AND sender_id = ?)
               OR (player_id = ? AND sender_id = ?)
            "#,
        )
        .bind(path_params.player_id)
        .bind(path_params.friend_id)
        .bind(path_params.friend_id)
        .bind(path_params.player_id)
        .execute(&mut *transaction)
        .await
        .map_err(log_database_error)?;
        transaction.commit().await.map_err(log_database_error)?;

        let friend = self
            .load_player(path_params.friend_id, can_read_discord_id(api_key))
            .await?
            .ok_or_else(|| "PlayerDB profile disappeared after friendship creation".to_string())?;

        Ok(AddPlayerFriendResponse::Status201_TheFriendWasAddedSuccessfully(friend))
    }

    async fn add_player_friend_request(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        path_params: &AddPlayerFriendRequestPathParams,
    ) -> Result<AddPlayerFriendRequestResponse, String> {
        if !api_key.has_scope(&ApiKeyScope::PlayersColonWrite) {
            return Ok(
                AddPlayerFriendRequestResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }

        if path_params.player_id == path_params.sender_id {
            return Ok(
                AddPlayerFriendRequestResponse::Status409_ThePlayerIsAlreadyFriendsWithTheSender,
            );
        }
        let Some((sender, receiver)) = self
            .load_friend_request_players(path_params.player_id, path_params.sender_id)
            .await?
        else {
            return Ok(AddPlayerFriendRequestResponse::Status404_ThePlayerOrSenderWasNotFound);
        };

        let (player1_id, player2_id) =
            normalize_friendship(path_params.player_id, path_params.sender_id);
        let already_friends = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM friendships
                WHERE player1_id = ? AND player2_id = ?
            )
            "#,
        )
        .bind(player1_id)
        .bind(player2_id)
        .fetch_one(&self.pool)
        .await
        .map_err(log_database_error)?;
        if already_friends {
            return Ok(
                AddPlayerFriendRequestResponse::Status409_ThePlayerIsAlreadyFriendsWithTheSender,
            );
        }

        let request = sqlx::query(
            r#"
            INSERT INTO friend_requests (player_id, sender_id)
            VALUES (?, ?)
            ON DUPLICATE KEY UPDATE created_at = created_at
            "#,
        )
        .bind(path_params.player_id)
        .bind(path_params.sender_id)
        .execute(&self.pool)
        .await
        .map_err(log_database_error)?;

        if request.rows_affected() > 0 {
            self.publish_stream_event(friend_request_added_event(sender, receiver))
                .await;
        }

        Ok(AddPlayerFriendRequestResponse::Status204_TheFriendRequestWasAddedSuccessfully)
    }

    async fn get_player_by_id(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        path_params: &GetPlayerByIdPathParams,
    ) -> Result<GetPlayerByIdResponse, String> {
        if !can_read_players(api_key) {
            return Ok(
                GetPlayerByIdResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }

        let Some(player) = self
            .load_player(path_params.player_id, can_read_discord_id(api_key))
            .await?
        else {
            return Ok(GetPlayerByIdResponse::Status404_ThePlayerWasNotFound);
        };

        Ok(GetPlayerByIdResponse::Status200_ThePlayerWasRetrievedSuccessfully(player))
    }

    async fn list_player_friend_requests(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        path_params: &ListPlayerFriendRequestsPathParams,
        query_params: &ListPlayerFriendRequestsQueryParams,
    ) -> Result<ListPlayerFriendRequestsResponse, String> {
        if !can_read_players(api_key) {
            return Ok(
                ListPlayerFriendRequestsResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }
        if self
            .player_db
            .get_username_by_uuid(path_params.player_id)
            .await?
            .is_none()
        {
            return Ok(ListPlayerFriendRequestsResponse::Status404_ThePlayerWasNotFound);
        }

        let limit = query_params.limit.unwrap_or(DEFAULT_PLAYERS_LIMIT);
        if !(1..=MAX_PLAYERS_LIMIT).contains(&limit) {
            return Ok(ListPlayerFriendRequestsResponse::Status400_TheRequestIsInvalid);
        }
        let limit = usize::from(limit);
        let cursor = match decode_player_cursor(query_params.cursor.as_deref()) {
            Ok(cursor) => cursor,
            Err(()) => {
                return Ok(ListPlayerFriendRequestsResponse::Status400_TheRequestIsInvalid);
            }
        };

        let mut query =
            QueryBuilder::<MySql>::new("SELECT sender_id FROM friend_requests WHERE player_id = ");
        query.push_bind(path_params.player_id);
        if let Some(cursor) = cursor {
            query.push(" AND sender_id > ").push_bind(cursor.value);
        }
        query
            .push(" ORDER BY sender_id ASC LIMIT ")
            .push_bind((limit + 1) as u64);

        let mut ids = query
            .build_query_scalar::<Uuid>()
            .fetch_all(&self.pool)
            .await
            .map_err(log_database_error)?;
        let next_cursor = take_next_cursor(&mut ids, limit)?;
        let items = self
            .load_players(&ids, can_read_discord_id(api_key))
            .await?;

        Ok(ListPlayerFriendRequestsResponse::Status200_ThePlayer(
            ListPlayers200Response::new(items, into_nullable(next_cursor)),
        ))
    }

    async fn list_player_friends(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        path_params: &ListPlayerFriendsPathParams,
        query_params: &ListPlayerFriendsQueryParams,
    ) -> Result<ListPlayerFriendsResponse, String> {
        if !can_read_players(api_key) {
            return Ok(
                ListPlayerFriendsResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }
        if self
            .player_db
            .get_username_by_uuid(path_params.player_id)
            .await?
            .is_none()
        {
            return Ok(ListPlayerFriendsResponse::Status404_ThePlayerWasNotFound);
        }
        let limit = query_params.limit.unwrap_or(DEFAULT_PLAYERS_LIMIT);
        if !(1..=MAX_PLAYERS_LIMIT).contains(&limit) {
            return Ok(ListPlayerFriendsResponse::Status400_TheRequestIsInvalid);
        }
        let limit = usize::from(limit);
        let cursor = match decode_player_cursor(query_params.cursor.as_deref()) {
            Ok(cursor) => cursor,
            Err(()) => return Ok(ListPlayerFriendsResponse::Status400_TheRequestIsInvalid),
        };

        let mut query = QueryBuilder::<MySql>::new(
            r#"
            SELECT CASE
                WHEN player1_id =
            "#,
        );
        query
            .push_bind(path_params.player_id)
            .push(
                " THEN player2_id ELSE player1_id END AS friend_id \
                   FROM friendships WHERE player1_id = ",
            )
            .push_bind(path_params.player_id)
            .push(" OR player2_id = ")
            .push_bind(path_params.player_id);
        if let Some(cursor) = cursor {
            query.push(" HAVING friend_id > ").push_bind(cursor.value);
        }
        query
            .push(" ORDER BY friend_id ASC LIMIT ")
            .push_bind((limit + 1) as u64);

        let mut ids = query
            .build_query_scalar::<Uuid>()
            .fetch_all(&self.pool)
            .await
            .map_err(log_database_error)?;
        let next_cursor = take_next_cursor(&mut ids, limit)?;
        let items = self
            .load_players(&ids, can_read_discord_id(api_key))
            .await?;

        Ok(ListPlayerFriendsResponse::Status200_ThePlayer(
            ListPlayers200Response::new(items, into_nullable(next_cursor)),
        ))
    }

    async fn list_players(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        query_params: &ListPlayersQueryParams,
    ) -> Result<ListPlayersResponse, String> {
        if !can_read_players(api_key) {
            return Ok(ListPlayersResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope);
        }

        let can_read_discord_id = can_read_discord_id(api_key);
        if query_params.discord_id.is_some() && !can_read_discord_id {
            return Ok(ListPlayersResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope);
        }

        let limit = query_params.limit.unwrap_or(DEFAULT_PLAYERS_LIMIT);
        if !(1..=MAX_PLAYERS_LIMIT).contains(&limit) {
            return Ok(ListPlayersResponse::Status400_TheRequestIsInvalid);
        }
        let limit = limit as usize;

        let cursor = match query_params.cursor.as_deref() {
            Some(cursor) => match PlayerCursor::decode(cursor) {
                Ok(cursor) if cursor.value == cursor.tie_breaker => Some(cursor),
                Err(_) => {
                    return Ok(ListPlayersResponse::Status400_TheRequestIsInvalid);
                }
                Ok(_) => {
                    return Ok(ListPlayersResponse::Status400_TheRequestIsInvalid);
                }
            },
            None => None,
        };
        let status = match query_params.status.as_deref() {
            Some(status) => match PlayerStatus::from_str(status) {
                Ok(status) => Some(status.to_string()),
                Err(_) => {
                    return Ok(ListPlayersResponse::Status400_TheRequestIsInvalid);
                }
            },
            None => None,
        };

        let mut query = QueryBuilder::<MySql>::new(
            "SELECT id, discord_id, status, current_server, bio \
             FROM players WHERE 1 = 1",
        );
        if let Some(discord_id) = &query_params.discord_id {
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

        let ids = rows.iter().map(|record| record.id).collect::<Vec<_>>();
        let items = self.load_players(&ids, can_read_discord_id).await?;

        Ok(
            ListPlayersResponse::Status200_ThePlayersWereRetrievedSuccessfully(
                ListPlayers200Response::new(items, into_nullable(next_cursor)),
            ),
        )
    }

    async fn reject_player_friend_request(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        path_params: &RejectPlayerFriendRequestPathParams,
    ) -> Result<RejectPlayerFriendRequestResponse, String> {
        if !api_key.has_scope(&ApiKeyScope::PlayersColonWrite) {
            return Ok(
                RejectPlayerFriendRequestResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }
        let Some((sender, receiver)) = self
            .load_friend_request_players(path_params.player_id, path_params.sender_id)
            .await?
        else {
            return Ok(RejectPlayerFriendRequestResponse::Status404_ThePlayer);
        };

        let request =
            sqlx::query("DELETE FROM friend_requests WHERE player_id = ? AND sender_id = ?")
                .bind(path_params.player_id)
                .bind(path_params.sender_id)
                .execute(&self.pool)
                .await
                .map_err(log_database_error)?;
        if request.rows_affected() == 0 {
            return Ok(RejectPlayerFriendRequestResponse::Status404_ThePlayer);
        }

        self.publish_stream_event(friend_request_rejected_event(sender, receiver))
            .await;
        Ok(RejectPlayerFriendRequestResponse::Status204_TheFriendRequestWasRejectedSuccessfully)
    }

    async fn remove_player_friend(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        path_params: &RemovePlayerFriendPathParams,
    ) -> Result<RemovePlayerFriendResponse, String> {
        if !api_key.has_scope(&ApiKeyScope::PlayersColonWrite) {
            return Ok(
                RemovePlayerFriendResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }

        if path_params.player_id == path_params.friend_id {
            return Ok(RemovePlayerFriendResponse::Status400_TheRequestIsInvalid);
        }
        if !self
            .players_exist(path_params.player_id, path_params.friend_id)
            .await?
        {
            return Ok(RemovePlayerFriendResponse::Status404_ThePlayerOrFriendWasNotFound);
        }

        let (player1_id, player2_id) =
            normalize_friendship(path_params.player_id, path_params.friend_id);

        let result = sqlx::query(
            r#"
            DELETE FROM friendships
            WHERE player1_id = ?
              AND player2_id = ?
            "#,
        )
        .bind(player1_id)
        .bind(player2_id)
        .execute(&self.pool)
        .await
        .map_err(log_database_error)?;

        if result.rows_affected() == 0 {
            return Ok(RemovePlayerFriendResponse::Status404_ThePlayerOrFriendWasNotFound);
        }

        Ok(RemovePlayerFriendResponse::Status204_TheFriendWasRemovedSuccessfully)
    }

    async fn remove_player_friend_request(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        path_params: &RemovePlayerFriendRequestPathParams,
    ) -> Result<RemovePlayerFriendRequestResponse, String> {
        if !api_key.has_scope(&ApiKeyScope::PlayersColonWrite) {
            return Ok(
                RemovePlayerFriendRequestResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }
        let Some((sender, receiver)) = self
            .load_friend_request_players(path_params.player_id, path_params.sender_id)
            .await?
        else {
            return Ok(RemovePlayerFriendRequestResponse::Status404_ThePlayerOrSenderWasNotFound);
        };

        let request =
            sqlx::query("DELETE FROM friend_requests WHERE player_id = ? AND sender_id = ?")
                .bind(path_params.player_id)
                .bind(path_params.sender_id)
                .execute(&self.pool)
                .await
                .map_err(log_database_error)?;
        if request.rows_affected() > 0 {
            self.publish_stream_event(friend_request_removed_event(sender, receiver))
                .await;
        }

        Ok(RemovePlayerFriendRequestResponse::Status204_TheFriendRequestWasRemovedSuccessfully)
    }

    async fn update_player_by_id(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        path_params: &UpdatePlayerByIdPathParams,
        body: &UpdatePlayerByIdRequest,
    ) -> Result<UpdatePlayerByIdResponse, String> {
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
            return Ok(UpdatePlayerByIdResponse::Status400_TheRequestIsInvalid);
        }
        let status = match &body.status {
            Some(status) => match PlayerStatus::from_str(status) {
                Ok(status) => Some(status.to_string()),
                Err(_) => {
                    return Ok(UpdatePlayerByIdResponse::Status400_TheRequestIsInvalid);
                }
            },
            None => None,
        };
        if body.bio.as_ref().is_some_and(|bio| {
            matches!(bio, graph_api::types::Nullable::Present(bio) if !(1..=160).contains(&bio.chars().count()))
        })
        {
            return Ok(UpdatePlayerByIdResponse::Status400_TheRequestIsInvalid);
        }

        let Some(username) = self
            .player_db
            .get_username_by_uuid(path_params.player_id)
            .await?
        else {
            return Ok(UpdatePlayerByIdResponse::Status404_ThePlayerWasNotFound);
        };

        let mut transaction = self.pool.begin().await.map_err(log_database_error)?;
        sqlx::query("INSERT IGNORE INTO players (id) VALUES (?)")
            .bind(path_params.player_id)
            .execute(&mut *transaction)
            .await
            .map_err(log_database_error)?;

        let mut query = QueryBuilder::<MySql>::new("UPDATE players SET ");
        let mut updates = query.separated(", ");
        if let Some(discord_id) = &body.discord_id {
            updates
                .push("discord_id = ")
                .push_bind_unseparated(from_nullable(discord_id));
        }
        if let Some(status) = status {
            updates.push("status = ").push_bind_unseparated(status);
        }
        if let Some(current_server) = &body.current_server {
            updates
                .push("current_server = ")
                .push_bind_unseparated(from_nullable(current_server));
        }
        if let Some(bio) = &body.bio {
            updates
                .push("bio = ")
                .push_bind_unseparated(from_nullable(bio));
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
                record.into_list_item(username, can_read_discord_id(api_key)),
            ),
        )
    }
}

fn can_read_players(api_key: &ApiKey) -> bool {
    api_key.has_scope(&ApiKeyScope::PlayersColonRead)
        || api_key.has_scope(&ApiKeyScope::PlayersColonReadDetails)
}

fn can_read_discord_id(api_key: &ApiKey) -> bool {
    api_key.has_scope(&ApiKeyScope::PlayersColonReadDetails)
}

fn normalize_friendship(a: Uuid, b: Uuid) -> (Uuid, Uuid) {
    if a < b { (a, b) } else { (b, a) }
}

fn decode_player_cursor(value: Option<&str>) -> Result<Option<PlayerCursor>, ()> {
    match value {
        Some(value) => match PlayerCursor::decode(value) {
            Ok(cursor) if cursor.value == cursor.tie_breaker => Ok(Some(cursor)),
            _ => Err(()),
        },
        None => Ok(None),
    }
}

fn take_next_cursor(ids: &mut Vec<Uuid>, limit: usize) -> Result<Option<String>, String> {
    if ids.len() <= limit {
        return Ok(None);
    }
    ids.pop();
    let last = *ids.last().expect("page must include at least one ID");
    PlayerCursor {
        value: last,
        tie_breaker: last,
    }
    .encode()
    .map(Some)
    .map_err(|error| {
        tracing::error!(?error, "failed to encode player cursor");
        "failed to encode player cursor".to_string()
    })
}

fn log_database_error(error: sqlx::Error) -> String {
    tracing::error!(?error, "player database operation failed");
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use graph_api::types::Nullable;

    #[test]
    fn player_conversion_hides_discord_id_without_details_scope() {
        let player = player_record().into_list_item("Steve".to_string(), false);

        assert_eq!(player.discord_id, Nullable::Null);
        assert_eq!(
            player.current_server,
            Nullable::Present("lobby".to_string())
        );
    }

    #[test]
    fn player_conversion_includes_discord_id_with_details_scope() {
        let player = player_record().into_list_item("Steve".to_string(), true);

        assert_eq!(
            player.discord_id,
            Nullable::Present("123456789012345678".to_string())
        );
    }

    #[test]
    fn empty_player_record_uses_public_defaults() {
        let id = Uuid::new_v4();
        let player = PlayerRecord::empty(id).into_list_item("Steve".to_string(), true);

        assert_eq!(player.id, id);
        assert_eq!(player.username, "Steve");
        assert_eq!(player.status, PlayerStatus::Offline.to_string());
        assert_eq!(player.discord_id, Nullable::Null);
        assert_eq!(player.current_server, Nullable::Null);
        assert_eq!(player.bio, Nullable::Null);
    }

    #[test]
    fn player_cursor_helpers_round_trip() {
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        let third = Uuid::from_u128(3);
        let mut ids = vec![first, second, third];

        let encoded = take_next_cursor(&mut ids, 2)
            .expect("cursor must be encoded")
            .expect("another page must be available");
        let cursor = decode_player_cursor(Some(&encoded))
            .expect("cursor must be valid")
            .expect("cursor must be present");

        assert_eq!(ids, vec![first, second]);
        assert_eq!(cursor.value, second);
        assert_eq!(cursor.tie_breaker, second);
    }

    #[test]
    fn player_cursor_rejects_different_tie_breaker() {
        let encoded = PlayerCursor {
            value: Uuid::from_u128(1),
            tie_breaker: Uuid::from_u128(2),
        }
        .encode()
        .expect("cursor must be encoded");

        assert!(decode_player_cursor(Some(&encoded)).is_err());
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

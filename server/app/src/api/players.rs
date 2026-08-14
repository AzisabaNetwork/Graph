use crate::api::Api;
use crate::api::filters::matches_half_open_range;
use crate::api::pagination::Cursor;
use crate::api::stream::{
    friend_added_event, friend_removed_event, friend_request_accepted_event,
    friend_request_added_event, friend_request_rejected_event, friend_request_removed_event,
};
use crate::auth::ApiKeyScopeChecker;
use crate::mojang::MojangProfileResolverError;
use crate::records::PlayerRecord;
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
use graph_api::types::Nullable;
use headers::Host;
use http::Method;
use sqlx::{MySql, MySqlPool, QueryBuilder};
use std::{collections::HashMap, str::FromStr};
use tokio::task::JoinSet;
use uuid::Uuid;

const DEFAULT_PLAYERS_LIMIT: u8 = 20;
const MAX_PLAYERS_LIMIT: u8 = 100;
const MAX_TRANSACTION_ATTEMPTS: usize = 3;

type PlayerCursor = Cursor<Uuid, Uuid>;

struct FriendRequestPlayers {
    receiver: Player,
    sender: Player,
}

impl PlayerRecord {
    fn matches_filters(
        &self,
        discord_id: Option<&str>,
        status: Option<&str>,
        cursor: Option<&PlayerCursor>,
        query_params: &ListPlayersQueryParams,
    ) -> bool {
        discord_id.is_none_or(|discord_id| self.discord_id.as_deref() == Some(discord_id))
            && status.is_none_or(|status| self.status == status)
            && query_params
                .current_server
                .as_deref()
                .is_none_or(|value| self.current_server.as_deref() == Some(value))
            && query_params
                .current_locale
                .as_deref()
                .is_none_or(|value| self.current_locale.as_deref() == Some(value))
            && query_params
                .current_client_version
                .as_deref()
                .is_none_or(|value| self.current_client_version.as_deref() == Some(value))
            && cursor.is_none_or(|cursor| self.id > cursor.value)
            && matches_half_open_range(
                self.first_login_at.as_ref(),
                query_params.first_login_from.as_ref(),
                query_params.first_login_to.as_ref(),
            )
            && matches_half_open_range(
                self.last_seen_at.as_ref(),
                query_params.last_seen_from.as_ref(),
                query_params.last_seen_to.as_ref(),
            )
    }
}

impl Api {
    async fn load_player_record(&self, id: Uuid) -> Result<PlayerRecord, String> {
        Ok(sqlx::query_as::<_, PlayerRecord>(
            r#"
            SELECT id, discord_id, status, current_server, current_locale,
                   current_client_version, bio, first_login_at, last_seen_at
            FROM players
            WHERE id = ?
            "#,
        )
        .bind(id)
        .fetch_optional(&self.default_pool)
        .await
        .map_err(log_database_error)?
        .unwrap_or_else(|| PlayerRecord::empty(id)))
    }

    async fn load_player(&self, id: Uuid, include_details: bool) -> Result<Option<Player>, String> {
        let Some(profile) = self.profile_resolver.find_by_uuid(id).await? else {
            return Ok(None);
        };
        let record = self.load_player_record(id).await?;
        Ok(Some(record.into_player(profile.username, include_details)))
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
            "SELECT id, discord_id, status, current_server, current_locale, \
             current_client_version, bio, first_login_at, last_seen_at \
             FROM players WHERE id IN (",
        );
        let mut separated = query.separated(", ");
        for id in ids {
            separated.push_bind(id);
        }
        separated.push_unseparated(")");

        let records = query
            .build_query_as::<PlayerRecord>()
            .fetch_all(&self.default_pool)
            .await
            .map_err(log_database_error)?;
        let mut records = records
            .into_iter()
            .map(|record| (record.id, record))
            .collect::<HashMap<_, _>>();

        let mut tasks = JoinSet::new();
        for (index, id) in ids.iter().copied().enumerate() {
            let profile_resolver = self.profile_resolver.clone();
            let record = records
                .remove(&id)
                .unwrap_or_else(|| PlayerRecord::empty(id));
            tasks.spawn(async move {
                let profile = profile_resolver.find_by_uuid(id).await?;
                Ok::<_, MojangProfileResolverError>((index, record, profile))
            });
        }

        let mut items = std::iter::repeat_with(|| None)
            .take(ids.len())
            .collect::<Vec<_>>();
        while let Some(result) = tasks.join_next().await {
            let (index, record, profile) = result
                .map_err(|error| format!("PlayerDB lookup task failed: {error}"))?
                .map_err(|error| {
                    tracing::error!(%error, "PlayerDB profile lookup failed");
                    error.to_string()
                })?;
            if let Some(profile) = profile {
                items[index] = Some(record.into_player(profile.username, include_details));
            }
        }

        Ok(items.into_iter().flatten().collect())
    }

    async fn players_exist(&self, first: Uuid, second: Uuid) -> Result<bool, String> {
        let (first, second) = tokio::try_join!(
            self.profile_resolver.find_by_uuid(first),
            self.profile_resolver.find_by_uuid(second),
        )?;
        Ok(first.is_some() && second.is_some())
    }

    async fn load_friend_request_players(
        &self,
        receiver_id: Uuid,
        sender_id: Uuid,
    ) -> Result<Option<FriendRequestPlayers>, String> {
        let (receiver, sender) = tokio::try_join!(
            self.load_player(receiver_id, false),
            self.load_player(sender_id, false),
        )?;
        match (receiver, sender) {
            (Some(receiver), Some(sender)) => Ok(Some(FriendRequestPlayers { receiver, sender })),
            (receiver, sender) => {
                tracing::warn!(
                    %receiver_id,
                    %sender_id,
                    receiver_found = receiver.is_some(),
                    sender_found = sender.is_some(),
                    "friend request participant was not found in PlayerDB"
                );
                Ok(None)
            }
        }
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
        let Some(players) = self
            .load_friend_request_players(path_params.player_id, path_params.sender_id)
            .await?
        else {
            return Ok(AcceptPlayerFriendRequestResponse::Status404_ThePlayer);
        };

        match create_friendship_with_retry(
            &self.default_pool,
            path_params.player_id,
            path_params.sender_id,
            FriendRequestDeletion::RequiredDirection,
        )
        .await
        .map_err(log_database_error)?
        {
            FriendshipWriteOutcome::Created => {}
            FriendshipWriteOutcome::RequestNotFound => {
                return Ok(AcceptPlayerFriendRequestResponse::Status404_ThePlayer);
            }
            FriendshipWriteOutcome::AlreadyExists => {
                return Ok(
                    AcceptPlayerFriendRequestResponse::Status409_ThePlayerIsAlreadyFriendsWithTheSender,
                );
            }
        }

        self.publish_stream_event(friend_request_accepted_event(
            players.sender.clone(),
            players.receiver.clone(),
        ))
        .await;
        self.publish_stream_event(friend_added_event(
            players.receiver.clone(),
            players.sender.clone(),
        ))
        .await;
        self.publish_stream_event(friend_added_event(players.sender, players.receiver))
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

        match create_friendship_with_retry(
            &self.default_pool,
            path_params.player_id,
            path_params.friend_id,
            FriendRequestDeletion::BothDirections,
        )
        .await
        .map_err(log_database_error)?
        {
            FriendshipWriteOutcome::Created => {}
            FriendshipWriteOutcome::AlreadyExists => {
                return Ok(
                    AddPlayerFriendResponse::Status409_ThePlayerIsAlreadyFriendsWithTheSpecifiedPlayer,
                );
            }
            FriendshipWriteOutcome::RequestNotFound => {
                unreachable!("friend requests are optional when adding a friend directly")
            }
        }

        let Some(players) = self
            .load_friend_request_players(path_params.player_id, path_params.friend_id)
            .await?
        else {
            return Err("PlayerDB profile disappeared after friendship creation".to_string());
        };

        self.publish_stream_event(friend_added_event(
            players.receiver.clone(),
            players.sender.clone(),
        ))
        .await;
        self.publish_stream_event(friend_added_event(players.sender, players.receiver))
            .await;

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
        let Some(players) = self
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
        .fetch_one(&self.default_pool)
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
        .execute(&self.default_pool)
        .await
        .map_err(log_database_error)?;

        if request.rows_affected() > 0 {
            self.publish_stream_event(friend_request_added_event(players.sender, players.receiver))
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
            .profile_resolver
            .find_by_uuid(path_params.player_id)
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
        if let Some(sender_id) = query_params.sender_id {
            query.push(" AND sender_id = ").push_bind(sender_id);
        }
        if let Some(cursor) = cursor {
            query.push(" AND sender_id > ").push_bind(cursor.value);
        }
        query
            .push(" ORDER BY sender_id ASC LIMIT ")
            .push_bind((limit + 1) as u64);

        let mut ids = query
            .build_query_scalar::<Uuid>()
            .fetch_all(&self.default_pool)
            .await
            .map_err(log_database_error)?;
        let next_cursor = take_next_cursor(&mut ids, limit)?;
        let items = self
            .load_players(&ids, can_read_discord_id(api_key))
            .await?;

        Ok(ListPlayerFriendRequestsResponse::Status200_ThePlayer(
            ListPlayers200Response::new(
                items,
                next_cursor.map_or(Nullable::Null, Nullable::Present),
            ),
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
            .profile_resolver
            .find_by_uuid(path_params.player_id)
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
                   FROM friendships WHERE (player1_id = ",
            )
            .push_bind(path_params.player_id)
            .push(" OR player2_id = ")
            .push_bind(path_params.player_id)
            .push(")");
        if query_params.friend_id.is_some() || cursor.is_some() {
            query.push(" HAVING ");
            let mut filters = query.separated(" AND ");
            if let Some(friend_id) = query_params.friend_id {
                filters
                    .push("friend_id = ")
                    .push_bind_unseparated(friend_id);
            }
            if let Some(cursor) = cursor {
                filters
                    .push("friend_id > ")
                    .push_bind_unseparated(cursor.value);
            }
        }
        query
            .push(" ORDER BY friend_id ASC LIMIT ")
            .push_bind((limit + 1) as u64);

        let mut ids = query
            .build_query_scalar::<Uuid>()
            .fetch_all(&self.default_pool)
            .await
            .map_err(log_database_error)?;
        let next_cursor = take_next_cursor(&mut ids, limit)?;
        let items = self
            .load_players(&ids, can_read_discord_id(api_key))
            .await?;

        Ok(ListPlayerFriendsResponse::Status200_ThePlayer(
            ListPlayers200Response::new(
                items,
                next_cursor.map_or(Nullable::Null, Nullable::Present),
            ),
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
        if let Some(username) = query_params.username.as_deref() {
            let profile = match self.profile_resolver.find_by_username(username).await? {
                Some(profile) => profile,
                None => {
                    return Ok(
                        ListPlayersResponse::Status200_ThePlayersWereRetrievedSuccessfully(
                            ListPlayers200Response::new(Vec::new(), Nullable::Null),
                        ),
                    );
                }
            };
            let record = self.load_player_record(profile.id).await?;
            let items = if !record.matches_filters(
                query_params.discord_id.as_deref(),
                status.as_deref(),
                cursor.as_ref(),
                query_params,
            ) {
                Vec::new()
            } else {
                vec![record.into_player(profile.username, can_read_discord_id)]
            };
            return Ok(
                ListPlayersResponse::Status200_ThePlayersWereRetrievedSuccessfully(
                    ListPlayers200Response::new(items, Nullable::Null),
                ),
            );
        }

        let mut query = QueryBuilder::<MySql>::new(
            "SELECT id, discord_id, status, current_server, current_locale, \
             current_client_version, bio, first_login_at, last_seen_at \
             FROM players WHERE 1 = 1",
        );
        if let Some(discord_id) = &query_params.discord_id {
            query.push(" AND discord_id = ").push_bind(discord_id);
        }
        if let Some(status) = status {
            query.push(" AND status = ").push_bind(status);
        }
        if let Some(current_server) = &query_params.current_server {
            query
                .push(" AND current_server = ")
                .push_bind(current_server);
        }
        if let Some(current_locale) = &query_params.current_locale {
            query
                .push(" AND current_locale = ")
                .push_bind(current_locale);
        }
        if let Some(current_client_version) = &query_params.current_client_version {
            query
                .push(" AND current_client_version = ")
                .push_bind(current_client_version);
        }
        if let Some(first_login_from) = &query_params.first_login_from {
            query
                .push(" AND first_login_at >= ")
                .push_bind(first_login_from);
        }
        if let Some(first_login_to) = &query_params.first_login_to {
            query
                .push(" AND first_login_at < ")
                .push_bind(first_login_to);
        }
        if let Some(last_seen_from) = &query_params.last_seen_from {
            query
                .push(" AND last_seen_at >= ")
                .push_bind(last_seen_from);
        }
        if let Some(last_seen_to) = &query_params.last_seen_to {
            query.push(" AND last_seen_at < ").push_bind(last_seen_to);
        }
        if let Some(cursor) = cursor {
            query.push(" AND id > ").push_bind(cursor.value);
        }
        query
            .push(" ORDER BY id ASC LIMIT ")
            .push_bind((limit + 1) as i64);

        let mut rows = query
            .build_query_as::<PlayerRecord>()
            .fetch_all(&self.default_pool)
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
                ListPlayers200Response::new(
                    items,
                    next_cursor.map_or(Nullable::Null, Nullable::Present),
                ),
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
        let Some(players) = self
            .load_friend_request_players(path_params.player_id, path_params.sender_id)
            .await?
        else {
            return Ok(RejectPlayerFriendRequestResponse::Status404_ThePlayer);
        };

        let request =
            sqlx::query("DELETE FROM friend_requests WHERE player_id = ? AND sender_id = ?")
                .bind(path_params.player_id)
                .bind(path_params.sender_id)
                .execute(&self.default_pool)
                .await
                .map_err(log_database_error)?;
        if request.rows_affected() == 0 {
            return Ok(RejectPlayerFriendRequestResponse::Status404_ThePlayer);
        }

        self.publish_stream_event(friend_request_rejected_event(
            players.sender,
            players.receiver,
        ))
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
        let Some(players) = self
            .load_friend_request_players(path_params.player_id, path_params.friend_id)
            .await?
        else {
            return Ok(RemovePlayerFriendResponse::Status404_ThePlayerOrFriendWasNotFound);
        };

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
        .execute(&self.default_pool)
        .await
        .map_err(log_database_error)?;

        if result.rows_affected() == 0 {
            return Ok(RemovePlayerFriendResponse::Status404_ThePlayerOrFriendWasNotFound);
        }

        self.publish_stream_event(friend_removed_event(players.receiver, players.sender))
            .await;

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
        let Some(players) = self
            .load_friend_request_players(path_params.player_id, path_params.sender_id)
            .await?
        else {
            return Ok(RemovePlayerFriendRequestResponse::Status404_ThePlayerOrSenderWasNotFound);
        };

        let request =
            sqlx::query("DELETE FROM friend_requests WHERE player_id = ? AND sender_id = ?")
                .bind(path_params.player_id)
                .bind(path_params.sender_id)
                .execute(&self.default_pool)
                .await
                .map_err(log_database_error)?;
        if request.rows_affected() > 0 {
            self.publish_stream_event(friend_request_removed_event(
                players.sender,
                players.receiver,
            ))
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
            && body.bio.is_none()
            && body.status.is_none()
            && body.current_server.is_none()
            && body.current_locale.is_none()
            && body.current_client_version.is_none()
            && body.first_login_at.is_none()
            && body.last_seen_at.is_none()
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

        let Some(profile) = self
            .profile_resolver
            .find_by_uuid(path_params.player_id)
            .await?
        else {
            return Ok(UpdatePlayerByIdResponse::Status404_ThePlayerWasNotFound);
        };

        let record = upsert_player_with_retry(
            &self.default_pool,
            path_params.player_id,
            body,
            status.as_deref(),
        )
        .await
        .map_err(log_database_error)?;

        Ok(
            UpdatePlayerByIdResponse::Status200_ThePlayerWasUpdatedSuccessfully(
                record.into_player(profile.username, can_read_discord_id(api_key)),
            ),
        )
    }
}

#[derive(Clone, Copy)]
enum FriendRequestDeletion {
    RequiredDirection,
    BothDirections,
}

enum FriendshipWriteOutcome {
    Created,
    RequestNotFound,
    AlreadyExists,
}

async fn create_friendship_with_retry(
    pool: &MySqlPool,
    player_id: Uuid,
    other_id: Uuid,
    deletion: FriendRequestDeletion,
) -> Result<FriendshipWriteOutcome, sqlx::Error> {
    for attempt in 1..=MAX_TRANSACTION_ATTEMPTS {
        match create_friendship_once(pool, player_id, other_id, deletion).await {
            Err(error) if is_mysql_deadlock(&error) && attempt < MAX_TRANSACTION_ATTEMPTS => {
                tracing::warn!(attempt, %player_id, %other_id, "retrying friendship update after deadlock");
            }
            result => return result,
        }
    }
    unreachable!("the final transaction attempt always returns")
}

async fn create_friendship_once(
    pool: &MySqlPool,
    player_id: Uuid,
    other_id: Uuid,
    deletion: FriendRequestDeletion,
) -> Result<FriendshipWriteOutcome, sqlx::Error> {
    let mut transaction = pool.begin().await?;

    let deleted = match deletion {
        FriendRequestDeletion::RequiredDirection => {
            sqlx::query("DELETE FROM friend_requests WHERE player_id = ? AND sender_id = ?")
                .bind(player_id)
                .bind(other_id)
                .execute(&mut *transaction)
                .await?
                .rows_affected()
        }
        FriendRequestDeletion::BothDirections => {
            // Delete the two primary-key rows in a stable order. More importantly,
            // every friendship-creation path now locks friend_requests before
            // friendships.
            let (first, second) = normalize_friendship(player_id, other_id);
            let mut deleted =
                sqlx::query("DELETE FROM friend_requests WHERE player_id = ? AND sender_id = ?")
                    .bind(first)
                    .bind(second)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
            deleted +=
                sqlx::query("DELETE FROM friend_requests WHERE player_id = ? AND sender_id = ?")
                    .bind(second)
                    .bind(first)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
            deleted
        }
    };

    if matches!(deletion, FriendRequestDeletion::RequiredDirection) && deleted == 0 {
        transaction.rollback().await?;
        return Ok(FriendshipWriteOutcome::RequestNotFound);
    }

    let (player1_id, player2_id) = normalize_friendship(player_id, other_id);
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
            transaction.rollback().await?;
            return Ok(FriendshipWriteOutcome::AlreadyExists);
        }
        Err(error) => return Err(error),
    }

    transaction.commit().await?;
    Ok(FriendshipWriteOutcome::Created)
}

async fn upsert_player_with_retry(
    pool: &MySqlPool,
    player_id: Uuid,
    body: &UpdatePlayerByIdRequest,
    status: Option<&str>,
) -> Result<PlayerRecord, sqlx::Error> {
    for attempt in 1..=MAX_TRANSACTION_ATTEMPTS {
        match upsert_player_once(pool, player_id, body, status).await {
            Err(error) if is_mysql_deadlock(&error) && attempt < MAX_TRANSACTION_ATTEMPTS => {
                tracing::warn!(attempt, %player_id, "retrying player update after deadlock");
            }
            result => return result,
        }
    }
    unreachable!("the final transaction attempt always returns")
}

async fn upsert_player_once(
    pool: &MySqlPool,
    player_id: Uuid,
    body: &UpdatePlayerByIdRequest,
    status: Option<&str>,
) -> Result<PlayerRecord, sqlx::Error> {
    let mut transaction = pool.begin().await?;

    // A single UPSERT avoids the shared-lock-to-exclusive-lock conversion caused by
    // `INSERT IGNORE` followed by `UPDATE` when concurrent PATCH requests target an
    // existing player.
    let mut query = QueryBuilder::<MySql>::new("INSERT INTO players (id");
    if body.discord_id.is_some() {
        query.push(", discord_id");
    }
    if body.bio.is_some() {
        query.push(", bio");
    }
    if status.is_some() {
        query.push(", status");
    }
    if body.current_server.is_some() {
        query.push(", current_server");
    }
    if body.current_locale.is_some() {
        query.push(", current_locale");
    }
    if body.current_client_version.is_some() {
        query.push(", current_client_version");
    }
    if body.first_login_at.is_some() {
        query.push(", first_login_at");
    }
    if body.last_seen_at.is_some() {
        query.push(", last_seen_at");
    }
    query.push(") VALUES (").push_bind(player_id);
    if let Some(value) = &body.discord_id {
        query.push(", ").push_bind(nullable_ref(value));
    }
    if let Some(value) = &body.bio {
        query.push(", ").push_bind(nullable_ref(value));
    }
    if let Some(value) = status {
        query.push(", ").push_bind(value);
    }
    if let Some(value) = &body.current_server {
        query.push(", ").push_bind(nullable_ref(value));
    }
    if let Some(value) = &body.current_locale {
        query.push(", ").push_bind(nullable_ref(value));
    }
    if let Some(value) = &body.current_client_version {
        query.push(", ").push_bind(nullable_ref(value));
    }
    if let Some(value) = &body.first_login_at {
        query.push(", ").push_bind(nullable_ref(value));
    }
    if let Some(value) = &body.last_seen_at {
        query.push(", ").push_bind(nullable_ref(value));
    }
    query.push(") ON DUPLICATE KEY UPDATE ");
    let mut updates = query.separated(", ");
    if body.discord_id.is_some() {
        updates.push("discord_id = VALUES(discord_id)");
    }
    if body.bio.is_some() {
        updates.push("bio = VALUES(bio)");
    }
    if status.is_some() {
        updates.push("status = VALUES(status)");
    }
    if body.current_server.is_some() {
        updates.push("current_server = VALUES(current_server)");
    }
    if body.current_locale.is_some() {
        updates.push("current_locale = VALUES(current_locale)");
    }
    if body.current_client_version.is_some() {
        updates.push("current_client_version = VALUES(current_client_version)");
    }
    if body.first_login_at.is_some() {
        updates.push("first_login_at = VALUES(first_login_at)");
    }
    if body.last_seen_at.is_some() {
        updates.push("last_seen_at = VALUES(last_seen_at)");
    }

    query.build().execute(&mut *transaction).await?;
    let record = sqlx::query_as::<_, PlayerRecord>(
        r#"
        SELECT id, discord_id, status, current_server, current_locale,
               current_client_version, bio, first_login_at, last_seen_at
        FROM players
        WHERE id = ?
        "#,
    )
    .bind(player_id)
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(record)
}

fn nullable_ref<T>(value: &Nullable<T>) -> Option<&T> {
    match value {
        Nullable::Present(value) => Some(value),
        Nullable::Null => None,
    }
}

fn is_mysql_deadlock(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(error) => error
            .try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()
            .is_some_and(|error| error.number() == 1213 && error.code() == Some("40001")),
        _ => false,
    }
}

fn can_read_players(api_key: &ApiKey) -> bool {
    api_key.has_any_scope(&[
        ApiKeyScope::PlayersColonRead,
        ApiKeyScope::PlayersColonReadDetails,
    ])
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
        let player = player_record().into_player("Steve".to_string(), false);

        assert_eq!(player.discord_id, Nullable::Null);
        assert_eq!(
            player.current_server,
            Nullable::Present("lobby".to_string())
        );
    }

    #[test]
    fn player_conversion_includes_discord_id_with_details_scope() {
        let player = player_record().into_player("Steve".to_string(), true);

        assert_eq!(
            player.discord_id,
            Nullable::Present("123456789012345678".to_string())
        );
    }

    #[test]
    fn empty_player_record_uses_public_defaults() {
        let id = Uuid::new_v4();
        let player = PlayerRecord::empty(id).into_player("Steve".to_string(), true);

        assert_eq!(player.id, id);
        assert_eq!(player.username, "Steve");
        assert_eq!(player.status, PlayerStatus::Offline.to_string());
        assert_eq!(player.discord_id, Nullable::Null);
        assert_eq!(player.current_server, Nullable::Null);
        assert_eq!(player.bio, Nullable::Null);
    }

    #[test]
    fn empty_player_record_matches_persistent_filters() {
        let id = Uuid::from_u128(2);
        let record = PlayerRecord::empty(id);
        let discord_id = Some("123456789012345678");
        let query = list_query_params();

        assert!(record.matches_filters(None, Some("offline"), None, &query));
        assert!(!record.matches_filters(discord_id, None, None, &query));
        assert!(!record.matches_filters(None, Some("online"), None, &query));
        assert!(!record.matches_filters(
            None,
            None,
            Some(&PlayerCursor {
                value: id,
                tie_breaker: id,
            }),
            &query,
        ));
    }

    #[test]
    fn player_record_matches_current_state_filters() {
        let record = player_record();
        let mut query = list_query_params();
        query.current_server = Some("lobby".to_string());
        query.current_locale = Some("ja_jp".to_string());
        query.current_client_version = Some("1.21.8".to_string());

        assert!(record.matches_filters(None, Some("online"), None, &query));
        query.current_server = Some("survival".to_string());
        assert!(!record.matches_filters(None, Some("online"), None, &query));
    }

    #[test]
    fn database_fields_map_to_current_player_state() {
        let player = player_record().into_player("Steve".to_string(), true);

        assert_eq!(player.status, PlayerStatus::Online.to_string());
        assert_eq!(
            player.current_server,
            Nullable::Present("lobby".to_string())
        );
        assert_eq!(
            player.current_locale,
            Nullable::Present("ja_jp".to_string())
        );
        assert_eq!(
            player.current_client_version,
            Nullable::Present("1.21.8".to_string())
        );
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_partial_player_upserts_preserve_all_fields() {
        let Ok(database_url) = std::env::var("GRAPH_TEST_DATABASE_URL") else {
            eprintln!("GRAPH_TEST_DATABASE_URL is not set; skipping MariaDB integration test");
            return;
        };
        let pool = MySqlPool::connect(&database_url).await.unwrap();
        sqlx::migrate!("../migrations").run(&pool).await.unwrap();

        let player_id = Uuid::new_v4();
        let workers = 24;
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(workers));
        let mut tasks = JoinSet::new();
        for index in 0..workers {
            let pool = pool.clone();
            let barrier = barrier.clone();
            tasks.spawn(async move {
                let mut body = UpdatePlayerByIdRequest::new();
                let status = if index % 2 == 0 {
                    body.status = Some(PlayerStatus::Online.to_string());
                    Some(PlayerStatus::Online.to_string())
                } else {
                    body.current_server = Some(Nullable::Present("lobby".to_string()));
                    None
                };
                barrier.wait().await;
                upsert_player_with_retry(&pool, player_id, &body, status.as_deref()).await
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap().unwrap();
        }

        let record = sqlx::query_as::<_, PlayerRecord>(
            r#"
            SELECT id, discord_id, status, current_server, current_locale,
                   current_client_version, bio, first_login_at, last_seen_at
            FROM players
            WHERE id = ?
            "#,
        )
        .bind(player_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(record.status, PlayerStatus::Online.to_string());
        assert_eq!(record.current_server.as_deref(), Some("lobby"));

        sqlx::query("DELETE FROM players WHERE id = ?")
            .bind(player_id)
            .execute(&pool)
            .await
            .unwrap();
    }

    fn player_record() -> PlayerRecord {
        PlayerRecord {
            id: Uuid::nil(),
            discord_id: Some("123456789012345678".to_string()),
            status: PlayerStatus::Online.to_string(),
            current_server: Some("lobby".to_string()),
            current_locale: Some("ja_jp".to_string()),
            current_client_version: Some("1.21.8".to_string()),
            bio: None,
            first_login_at: None,
            last_seen_at: None,
        }
    }

    fn list_query_params() -> ListPlayersQueryParams {
        ListPlayersQueryParams {
            limit: None,
            cursor: None,
            username: None,
            discord_id: None,
            status: None,
            current_server: None,
            current_locale: None,
            current_client_version: None,
            first_login_from: None,
            first_login_to: None,
            last_seen_from: None,
            last_seen_to: None,
        }
    }
}

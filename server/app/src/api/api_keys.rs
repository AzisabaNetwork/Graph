use crate::api::{Api, into_nullable};
use crate::auth::credentials::ApiKeyCredentials;
use crate::auth::scope::ApiKeyScopeExt;
use crate::pagination::Cursor;
use async_trait::async_trait;
use axum_extra::extract::CookieJar;
use chrono::{DateTime, Utc};
use graph_api::apis::api_keys::{
    ApiKeys, CreateApiKeyResponse, DeleteApiKeyByIdResponse, GetApiKeyByIdResponse,
    ListApiKeysResponse, UpdateApiKeyByIdResponse,
};
use graph_api::models::{
    ApiKey, ApiKeyScope, CreateApiKey201Response, CreateApiKeyRequest, DeleteApiKeyByIdPathParams,
    GetApiKeyByIdPathParams, ListApiKeys200Response, ListApiKeysQueryParams,
    UpdateApiKeyByIdPathParams, UpdateApiKeyByIdRequest,
};
use headers::Host;
use http::Method;
use sqlx::{FromRow, MySql, QueryBuilder};
use std::collections::{BTreeMap, BTreeSet};

const DEFAULT_API_KEYS_LIMIT: u8 = 20;
const MAX_API_KEYS_LIMIT: u8 = 100;

type ApiKeyCursor = Cursor<DateTime<Utc>, String>;

#[derive(Debug, FromRow)]
struct ApiKeyRecord {
    public_id: String,
    name: String,
    created_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    player_id: Option<uuid::Uuid>,
}

impl ApiKeyRecord {
    fn into_list_item(self, scopes: &BTreeMap<String, Vec<String>>) -> ApiKey {
        let ApiKeyRecord {
            public_id,
            name,
            created_at,
            expires_at,
            player_id,
        } = self;

        let item_scopes = scopes.get(&public_id).cloned().unwrap_or_default();

        ApiKey::new(
            name,
            public_id,
            item_scopes,
            created_at,
            into_nullable(expires_at),
            into_nullable(player_id),
        )
    }
}

#[async_trait]
impl ApiKeys<String> for Api {
    type Claims = ApiKey;

    async fn create_api_key(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        body: &CreateApiKeyRequest,
    ) -> Result<CreateApiKeyResponse, String> {
        if !api_key.has_scope(&ApiKeyScope::ApiKeysColonWrite) {
            return Ok(CreateApiKeyResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope);
        }

        if !(1..=100).contains(&body.name.chars().count()) {
            return Ok(CreateApiKeyResponse::Status400_TheRequestIsInvalid);
        }

        let requested_scopes = match parse_api_key_scopes(&body.scopes) {
            Some(scopes) => scopes,
            None => return Ok(CreateApiKeyResponse::Status400_TheRequestIsInvalid),
        };
        if !api_key.has_all_scopes(&requested_scopes) {
            return Ok(CreateApiKeyResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope);
        }

        let requested_player_id = body
            .player_id
            .as_ref()
            .and_then(|player_id| match player_id {
                graph_api::types::Nullable::Present(player_id) => Some(*player_id),
                graph_api::types::Nullable::Null => None,
            });
        let player_id = match delegated_player_id(api_key, requested_player_id) {
            Ok(player_id) => player_id,
            Err(()) => {
                return Ok(
                    CreateApiKeyResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
                );
            }
        };

        let created_at = Utc::now();
        let expires_at = body
            .expires_at
            .as_ref()
            .and_then(|expires_at| match expires_at {
                graph_api::types::Nullable::Present(expires_at) => Some(*expires_at),
                graph_api::types::Nullable::Null => None,
            });
        if expires_at.is_some_and(|expires_at| expires_at <= created_at) {
            return Ok(CreateApiKeyResponse::Status400_TheRequestIsInvalid);
        }

        let credentials = ApiKeyCredentials::generate().map_err(|error| {
            tracing::error!(?error, "failed to generate API key credentials");
            "failed to generate API key credentials".to_string()
        })?;
        let secret_digest = credentials.secret_digest();
        let public_id = credentials.public_id().to_owned();
        let token = credentials.to_token();
        let scopes = requested_scopes
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        let mut transaction = self.pool.begin().await.map_err(log_database_error)?;
        sqlx::query(
            r#"
            INSERT INTO api_keys
                (public_id, created_by_public_id, name, secret_digest, created_at, expires_at)
            VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&public_id)
        .bind(&api_key.public_id)
        .bind(&body.name)
        .bind(secret_digest.as_slice())
        .bind(created_at)
        .bind(expires_at)
        .execute(&mut *transaction)
        .await
        .map_err(log_database_error)?;

        for scope in &scopes {
            sqlx::query("INSERT INTO api_key_scopes (api_key_public_id, scope) VALUES (?, ?)")
                .bind(&public_id)
                .bind(scope)
                .execute(&mut *transaction)
                .await
                .map_err(log_database_error)?;
        }

        if let Some(player_id) = player_id {
            sqlx::query("INSERT INTO api_key_players (api_key_public_id, player_id) VALUES (?, ?)")
                .bind(&public_id)
                .bind(player_id)
                .execute(&mut *transaction)
                .await
                .map_err(log_database_error)?;
        }

        transaction.commit().await.map_err(log_database_error)?;

        Ok(
            CreateApiKeyResponse::Status201_TheAPIKeyWasCreatedSuccessfully(
                CreateApiKey201Response::new(
                    body.name.clone(),
                    public_id,
                    scopes,
                    created_at,
                    into_nullable(expires_at),
                    into_nullable(player_id),
                    token,
                ),
            ),
        )
    }

    async fn delete_api_key_by_id(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        path_params: &DeleteApiKeyByIdPathParams,
    ) -> Result<DeleteApiKeyByIdResponse, String> {
        if !api_key.has_scope(&ApiKeyScope::ApiKeysColonWrite) {
            return Ok(
                DeleteApiKeyByIdResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }

        if path_params.api_key_id == api_key.public_id {
            return Ok(
                DeleteApiKeyByIdResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }

        if !self.delete_api_key_tree(&path_params.api_key_id).await? {
            return Ok(DeleteApiKeyByIdResponse::Status404_TheAPIKeyWasNotFound);
        }

        Ok(DeleteApiKeyByIdResponse::Status204_TheAPIKeyWasDeletedSuccessfully)
    }

    async fn get_api_key_by_id(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        path_params: &GetApiKeyByIdPathParams,
    ) -> Result<GetApiKeyByIdResponse, String> {
        if !api_key.has_scope(&ApiKeyScope::ApiKeysColonRead) {
            return Ok(
                GetApiKeyByIdResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }

        let record = sqlx::query_as::<_, ApiKeyRecord>(
            r#"
            SELECT k.public_id, k.name, k.created_at, k.expires_at, p.player_id
            FROM api_keys k
            LEFT JOIN api_key_players p ON p.api_key_public_id = k.public_id
            WHERE k.public_id = ?
            "#,
        )
        .bind(&path_params.api_key_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_database_error)?;

        let Some(record) = record else {
            return Ok(GetApiKeyByIdResponse::Status404_TheAPIKeyWasNotFound);
        };
        let scopes = self
            .load_api_key_scopes(std::slice::from_ref(&path_params.api_key_id))
            .await?;

        Ok(
            GetApiKeyByIdResponse::Status200_TheAPIKeyWasRetrievedSuccessfully(
                record.into_list_item(&scopes),
            ),
        )
    }

    async fn list_api_keys(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        query_params: &ListApiKeysQueryParams,
    ) -> Result<ListApiKeysResponse, String> {
        if !api_key.has_scope(&ApiKeyScope::ApiKeysColonRead) {
            return Ok(ListApiKeysResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope);
        }

        let limit = query_params.limit.unwrap_or(DEFAULT_API_KEYS_LIMIT);
        if !(1..=MAX_API_KEYS_LIMIT).contains(&limit) {
            return Ok(ListApiKeysResponse::Status400_TheRequestIsInvalid);
        }
        let limit = limit as usize;
        let cursor = match query_params.cursor.as_deref() {
            Some(cursor) => match ApiKeyCursor::decode(cursor) {
                Ok(cursor) => Some(cursor),
                Err(_) => {
                    return Ok(ListApiKeysResponse::Status400_TheRequestIsInvalid);
                }
            },
            None => None,
        };

        let mut query = QueryBuilder::<MySql>::new(
            "SELECT k.public_id, k.name, k.created_at, k.expires_at, p.player_id FROM api_keys k LEFT JOIN api_key_players p ON p.api_key_public_id = k.public_id WHERE 1 = 1",
        );
        if let Some(cursor) = cursor {
            query
                .push(" AND (k.created_at < ")
                .push_bind(cursor.value)
                .push(" OR (k.created_at = ")
                .push_bind(cursor.value)
                .push(" AND k.public_id < ")
                .push_bind(cursor.tie_breaker)
                .push("))");
        }
        query
            .push(" ORDER BY k.created_at DESC, k.public_id DESC LIMIT ")
            .push_bind((limit + 1) as i64);

        let mut rows = query
            .build_query_as::<ApiKeyRecord>()
            .fetch_all(&self.pool)
            .await
            .map_err(log_database_error)?;

        let next_cursor = if rows.len() > limit {
            rows.pop();
            let last = rows.last().expect("page must include at least one row");
            Some(
                ApiKeyCursor {
                    value: last.created_at,
                    tie_breaker: last.public_id.clone(),
                }
                .encode()
                .map_err(|error| {
                    tracing::error!(?error, "failed to encode API keys cursor");
                    "failed to encode API keys cursor".to_string()
                })?,
            )
        } else {
            None
        };

        let public_ids = rows
            .iter()
            .map(|record| record.public_id.clone())
            .collect::<Vec<_>>();
        let scopes = self.load_api_key_scopes(&public_ids).await?;
        let items = rows
            .into_iter()
            .map(|record| record.into_list_item(&scopes))
            .collect();

        Ok(
            ListApiKeysResponse::Status200_TheAPIKeysWereRetrievedSuccessfully(
                ListApiKeys200Response::new(items, into_nullable(next_cursor)),
            ),
        )
    }

    async fn update_api_key_by_id(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        path_params: &UpdateApiKeyByIdPathParams,
        body: &UpdateApiKeyByIdRequest,
    ) -> Result<UpdateApiKeyByIdResponse, String> {
        if !api_key.has_scope(&ApiKeyScope::ApiKeysColonWrite) {
            return Ok(
                UpdateApiKeyByIdResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }

        if body
            .name
            .as_ref()
            .is_some_and(|name| !(1..=100).contains(&name.chars().count()))
        {
            return Ok(UpdateApiKeyByIdResponse::Status400_TheRequestIsInvalid);
        }

        let requested_scopes = match body.scopes.as_ref() {
            Some(scopes) => {
                let Some(scopes) = parse_api_key_scopes(scopes) else {
                    return Ok(UpdateApiKeyByIdResponse::Status400_TheRequestIsInvalid);
                };

                if !api_key.has_all_scopes(&scopes) {
                    return Ok(UpdateApiKeyByIdResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope);
                }

                Some(scopes)
            }
            None => None,
        };

        let requested_expires_at = match body.expires_at.as_ref() {
            Some(graph_api::types::Nullable::Present(expires_at)) => Some(Some(*expires_at)),
            Some(graph_api::types::Nullable::Null) => Some(None),
            None => None,
        };

        let requested_player_id = match body.player_id.as_ref() {
            Some(graph_api::types::Nullable::Present(player_id)) => Some(Some(*player_id)),
            Some(graph_api::types::Nullable::Null) => Some(None),
            None => None,
        };

        let mut transaction = self.pool.begin().await.map_err(log_database_error)?;

        let exists = sqlx::query_scalar::<_, String>(
            "SELECT public_id FROM api_keys WHERE public_id = ? FOR UPDATE",
        )
        .bind(&path_params.api_key_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(log_database_error)?;

        if exists.is_none() {
            return Ok(UpdateApiKeyByIdResponse::Status404_TheAPIKeyWasNotFound);
        }

        if let Some(name) = &body.name {
            sqlx::query("UPDATE api_keys SET name = ? WHERE public_id = ?")
                .bind(name)
                .bind(&path_params.api_key_id)
                .execute(&mut *transaction)
                .await
                .map_err(log_database_error)?;
        }

        if let Some(expires_at) = requested_expires_at {
            sqlx::query("UPDATE api_keys SET expires_at = ? WHERE public_id = ?")
                .bind(expires_at)
                .bind(&path_params.api_key_id)
                .execute(&mut *transaction)
                .await
                .map_err(log_database_error)?;
        }

        if let Some(scopes) = requested_scopes {
            sqlx::query("DELETE FROM api_key_scopes WHERE api_key_public_id = ?")
                .bind(&path_params.api_key_id)
                .execute(&mut *transaction)
                .await
                .map_err(log_database_error)?;

            for scope in scopes {
                sqlx::query("INSERT INTO api_key_scopes (api_key_public_id, scope) VALUES (?, ?)")
                    .bind(&path_params.api_key_id)
                    .bind(scope.to_string())
                    .execute(&mut *transaction)
                    .await
                    .map_err(log_database_error)?;
            }
        }

        if let Some(player_id) = requested_player_id {
            sqlx::query("DELETE FROM api_key_players WHERE api_key_public_id = ?")
                .bind(&path_params.api_key_id)
                .execute(&mut *transaction)
                .await
                .map_err(log_database_error)?;

            if let Some(player_id) = player_id {
                sqlx::query(
                    r#"INSERT INTO api_key_players (api_key_public_id, player_id)
                    VALUES (?, ?)
                    "#,
                )
                .bind(&path_params.api_key_id)
                .bind(player_id)
                .execute(&mut *transaction)
                .await
                .map_err(log_database_error)?;
            }
        }

        transaction.commit().await.map_err(log_database_error)?;

        let record = sqlx::query_as::<_, ApiKeyRecord>(
            r#"
            SELECT k.public_id, k.name, k.created_at, k.expires_at, p.player_id
            FROM api_keys k
            LEFT JOIN api_key_players p ON p.api_key_public_id = k.public_id
            WHERE k.public_id = ?
            "#,
        )
        .bind(&path_params.api_key_id)
        .fetch_one(&self.pool)
        .await
        .map_err(log_database_error)?;

        let scopes = self
            .load_api_key_scopes(std::slice::from_ref(&path_params.api_key_id))
            .await?;

        Ok(
            UpdateApiKeyByIdResponse::Status200_TheAPIKeyWasUpdatedSuccessfully(
                record.into_list_item(&scopes),
            ),
        )
    }
}

impl Api {
    pub(crate) async fn provision_bootstrap_api_key(&self, token: &str) -> Result<(), String> {
        let credentials = token
            .parse::<ApiKeyCredentials>()
            .map_err(|error| format!("GRAPH_BOOTSTRAP_API_KEY is invalid: {error}"))?;
        let secret_digest = credentials.secret_digest();

        let mut transaction = self.pool.begin().await.map_err(log_database_error)?;
        sqlx::query(
            r#"
            INSERT INTO api_keys
                (public_id, created_by_public_id, name, secret_digest, created_at, expires_at)
            VALUES (?, NULL, 'Bootstrap administrator', ?, UTC_TIMESTAMP(6), NULL)
            ON DUPLICATE KEY UPDATE
                created_by_public_id = NULL,
                name = VALUES(name),
                secret_digest = VALUES(secret_digest),
                expires_at = NULL
            "#,
        )
        .bind(credentials.public_id())
        .bind(secret_digest.as_slice())
        .execute(&mut *transaction)
        .await
        .map_err(log_database_error)?;

        sqlx::query("DELETE FROM api_key_players WHERE api_key_public_id = ?")
            .bind(credentials.public_id())
            .execute(&mut *transaction)
            .await
            .map_err(log_database_error)?;

        sqlx::query("DELETE FROM api_key_scopes WHERE api_key_public_id = ?")
            .bind(credentials.public_id())
            .execute(&mut *transaction)
            .await
            .map_err(log_database_error)?;
        sqlx::query("INSERT INTO api_key_scopes (api_key_public_id, scope) VALUES (?, ?)")
            .bind(credentials.public_id())
            .bind(ApiKeyScope::Star.to_string())
            .execute(&mut *transaction)
            .await
            .map_err(log_database_error)?;

        transaction.commit().await.map_err(log_database_error)
    }

    async fn delete_api_key_tree(&self, root_public_id: &str) -> Result<bool, String> {
        let mut transaction = self.pool.begin().await.map_err(log_database_error)?;
        let root = sqlx::query_scalar::<_, String>(
            "SELECT public_id FROM api_keys WHERE public_id = ? FOR UPDATE",
        )
        .bind(root_public_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(log_database_error)?;
        let Some(root) = root else {
            return Ok(false);
        };

        let mut pending = vec![root];
        let mut deletion_order = Vec::new();
        let mut visited = BTreeSet::new();
        while let Some(public_id) = pending.pop() {
            if !visited.insert(public_id.clone()) {
                tracing::error!(%public_id, "cycle detected in API key creation tree");
                return Err("cycle detected in API key creation tree".to_string());
            }

            let children = sqlx::query_scalar::<_, String>(
                r#"
                SELECT public_id
                FROM api_keys
                WHERE created_by_public_id = ?
                FOR UPDATE
                "#,
            )
            .bind(&public_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(log_database_error)?;
            pending.extend(children);
            deletion_order.push(public_id);
        }

        // RESTRICT protects the tree from orphaned rows, so descendants must be
        // deleted before their creators. Reversing the traversal provides that order.
        for public_id in deletion_order.into_iter().rev() {
            sqlx::query("DELETE FROM api_keys WHERE public_id = ?")
                .bind(public_id)
                .execute(&mut *transaction)
                .await
                .map_err(log_database_error)?;
        }

        transaction.commit().await.map_err(log_database_error)?;
        Ok(true)
    }

    async fn load_api_key_scopes(
        &self,
        public_ids: &[String],
    ) -> Result<BTreeMap<String, Vec<String>>, String> {
        if public_ids.is_empty() {
            return Ok(BTreeMap::new());
        }

        let mut query = QueryBuilder::<MySql>::new(
            "SELECT api_key_public_id, scope FROM api_key_scopes WHERE api_key_public_id IN (",
        );
        let mut separated = query.separated(", ");
        for public_id in public_ids {
            separated.push_bind(public_id);
        }
        separated.push_unseparated(") ORDER BY api_key_public_id, scope");

        let rows = query
            .build_query_as::<(String, String)>()
            .fetch_all(&self.pool)
            .await
            .map_err(log_database_error)?;
        let mut scopes = BTreeMap::<String, Vec<String>>::new();
        for (public_id, scope) in rows {
            scopes.entry(public_id).or_default().push(scope);
        }
        Ok(scopes)
    }
}

fn parse_api_key_scopes(scopes: &[String]) -> Option<Vec<ApiKeyScope>> {
    let parsed = scopes
        .iter()
        .map(|scope| scope.parse())
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let unique = parsed.iter().copied().collect::<BTreeSet<_>>();
    (unique.len() == parsed.len()).then_some(parsed)
}

fn delegated_player_id(
    api_key: &ApiKey,
    requested: Option<uuid::Uuid>,
) -> Result<Option<uuid::Uuid>, ()> {
    match api_key.player_id {
        graph_api::types::Nullable::Present(player_id) => {
            if requested.is_some_and(|requested| requested != player_id) {
                Err(())
            } else {
                Ok(Some(player_id))
            }
        }
        graph_api::types::Nullable::Null => {
            if requested.is_some() && !api_key.scopes.iter().any(|scope| scope == "*") {
                Err(())
            } else {
                Ok(requested)
            }
        }
    }
}

fn log_database_error(error: sqlx::Error) -> String {
    tracing::error!(?error, "API key database operation failed");
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mojang::PlayerDbClient;
    use graph_api::types::Nullable;
    use headers::Header;

    fn test_host() -> Host {
        let value = http::HeaderValue::from_static("localhost");
        Host::decode(&mut std::iter::once(&value)).unwrap()
    }

    fn key(scopes: &[&str], player_id: Option<uuid::Uuid>) -> ApiKey {
        ApiKey::new(
            "test".to_string(),
            "test".to_string(),
            scopes.iter().map(|scope| (*scope).to_string()).collect(),
            Utc::now(),
            Nullable::Null,
            into_nullable(player_id),
        )
    }

    #[test]
    fn player_key_children_inherit_player() {
        let player = uuid::Uuid::new_v4();
        assert_eq!(
            delegated_player_id(&key(&["api-keys:write"], Some(player)), None),
            Ok(Some(player))
        );
        assert_eq!(
            delegated_player_id(&key(&["api-keys:write"], Some(player)), Some(player)),
            Ok(Some(player))
        );
    }

    #[test]
    fn player_key_cannot_delegate_another_player() {
        assert_eq!(
            delegated_player_id(
                &key(&["*"], Some(uuid::Uuid::new_v4())),
                Some(uuid::Uuid::new_v4())
            ),
            Err(())
        );
    }

    #[test]
    fn only_unlinked_star_key_can_assign_player() {
        let player = uuid::Uuid::new_v4();
        assert_eq!(
            delegated_player_id(&key(&["*"], None), Some(player)),
            Ok(Some(player))
        );
        assert_eq!(
            delegated_player_id(&key(&["api-keys:write"], None), Some(player)),
            Err(())
        );
        assert_eq!(
            delegated_player_id(&key(&["api-keys:write"], None), None),
            Ok(None)
        );
    }

    #[tokio::test]
    async fn mariadb_player_link_is_persisted_and_loaded() {
        let Ok(database_url) = std::env::var("GRAPH_TEST_DATABASE_URL") else {
            eprintln!("GRAPH_TEST_DATABASE_URL is not set; skipping MariaDB integration test");
            return;
        };
        let pool = sqlx::MySqlPool::connect(&database_url).await.unwrap();
        sqlx::migrate!("../migrations").run(&pool).await.unwrap();

        let api = Api::new(
            pool.clone(),
            pool.clone(),
            None,
            PlayerDbClient::new().unwrap(),
        );
        let credentials = ApiKeyCredentials::generate().unwrap();
        api.provision_bootstrap_api_key(&credentials.to_token())
            .await
            .unwrap();
        let player_id = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO players (id) VALUES (?)")
            .bind(player_id)
            .execute(&pool)
            .await
            .unwrap();

        let parent = ApiKey::new(
            "bootstrap".to_string(),
            credentials.public_id().to_string(),
            vec!["*".to_string()],
            Utc::now(),
            Nullable::Null,
            Nullable::Null,
        );
        let mut request = CreateApiKeyRequest::new(
            "linked integration key".to_string(),
            vec!["punishments:write".to_string()],
        );
        request.player_id = Some(Nullable::Present(player_id));
        let response = api
            .create_api_key(
                &Method::POST,
                &test_host(),
                &CookieJar::new(),
                &parent,
                &request,
            )
            .await
            .unwrap();
        let created = match response {
            CreateApiKeyResponse::Status201_TheAPIKeyWasCreatedSuccessfully(created) => created,
            response => panic!("unexpected response: {response:?}"),
        };
        assert_eq!(created.player_id, Nullable::Present(player_id));

        let stored_player_id = sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT player_id FROM api_key_players WHERE api_key_public_id = ?",
        )
        .bind(&created.public_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored_player_id, player_id);

        let loaded = api
            .get_api_key_by_id(
                &Method::GET,
                &test_host(),
                &CookieJar::new(),
                &parent,
                &GetApiKeyByIdPathParams {
                    api_key_id: created.public_id.clone(),
                },
            )
            .await
            .unwrap();
        match loaded {
            GetApiKeyByIdResponse::Status200_TheAPIKeyWasRetrievedSuccessfully(loaded) => {
                assert_eq!(loaded.player_id, Nullable::Present(player_id));
            }
            response => panic!("unexpected response: {response:?}"),
        }

        sqlx::query("DELETE FROM api_keys WHERE public_id = ?")
            .bind(&created.public_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM players WHERE id = ?")
            .bind(player_id)
            .execute(&pool)
            .await
            .unwrap();
        api.delete_api_key_tree(credentials.public_id())
            .await
            .unwrap();
    }
}

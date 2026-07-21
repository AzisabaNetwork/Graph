use crate::api::{Api, into_nullable};
use crate::auth::credentials::ApiKeyCredentials;
use async_trait::async_trait;
use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use graph_api::apis::{ApiAuthBasic, BasicAuthKind};
use graph_api::models::ApiKey;
use graph_api::types::Nullable;
use http::header::AUTHORIZATION;
use sqlx::FromRow;

#[derive(Debug, FromRow)]
struct ApiKeyRecord {
    name: String,
    public_id: String,
    secret_digest: Vec<u8>,
    created_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    player_id: Option<uuid::Uuid>,
}

#[async_trait]
impl ApiAuthBasic for Api {
    type Claims = ApiKey;

    async fn extract_claims_from_auth_header(
        &self,
        kind: BasicAuthKind,
        headers: &HeaderMap,
        _key: &str,
    ) -> Option<Self::Claims> {
        if !matches!(kind, BasicAuthKind::Bearer) {
            return None;
        }

        let authorization = headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())?;

        let credentials = ApiKeyCredentials::from_authorization_header(authorization).ok()?;

        let api_key = match sqlx::query_as::<_, ApiKeyRecord>(
            r#"
            SELECT k.name, k.public_id, k.secret_digest, k.created_at, k.expires_at, p.player_id
            FROM api_keys k
            LEFT JOIN api_key_players p ON p.api_key_public_id = k.public_id
            WHERE k.public_id = ?
            "#,
        )
        .bind(credentials.public_id())
        .fetch_optional(self.pool())
        .await
        {
            Ok(Some(api_key)) => api_key,
            Ok(None) => return None,
            Err(error) => {
                tracing::error!(error = %error, "failed to verify API key");
                return None;
            }
        };

        if !credentials.matches_digest(&api_key.secret_digest)
            || api_key
                .expires_at
                .is_some_and(|expires_at| expires_at <= Utc::now())
        {
            return None;
        }

        let scopes = match sqlx::query_scalar::<_, String>(
            r#"
            SELECT scope
            FROM api_key_scopes
            WHERE api_key_public_id = ?
            "#,
        )
        .bind(&api_key.public_id)
        .fetch_all(self.pool())
        .await
        {
            Ok(scopes) => scopes,
            Err(error) => {
                tracing::error!(error = %error, "failed to load API key scopes");
                return None;
            }
        };

        let expires_at = api_key.expires_at.map_or(Nullable::Null, Nullable::Present);

        Some(ApiKey::new(
            api_key.name,
            api_key.public_id,
            scopes,
            api_key.created_at,
            expires_at,
            into_nullable(api_key.player_id),
        ))
    }
}

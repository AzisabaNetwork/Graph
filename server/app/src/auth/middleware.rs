use crate::api::Api;
use crate::auth::credentials::ApiKeyCredentials;
use axum::extract::Request;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use graph_api::models::ApiKey;
use graph_api::types::Nullable;
use http::StatusCode;
use http::header::AUTHORIZATION;
use sqlx::FromRow;
use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::{Layer, Service};

#[derive(Clone, Debug)]
pub(crate) struct ApiKeyAuthLayer {
    api: Api,
}

#[derive(Clone, Debug)]
pub(crate) struct ApiKeyAuthService<S> {
    api: Api,
    inner: S,
}

#[derive(Debug)]
enum ApiKeyVerificationError {
    InvalidCredentials,
    Database(sqlx::Error),
}

#[derive(Debug, FromRow)]
struct ApiKeyRecord {
    name: String,
    public_id: String,
    secret_digest: Vec<u8>,
    created_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
}

impl ApiKeyAuthLayer {
    pub(crate) fn new(api: Api) -> Self {
        Self { api }
    }
}

impl<S> Layer<S> for ApiKeyAuthLayer {
    type Service = ApiKeyAuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ApiKeyAuthService {
            api: self.api.clone(),
            inner,
        }
    }
}

impl<S> Service<Request> for ApiKeyAuthService<S>
where
    S: Service<Request, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(context)
    }

    fn call(&mut self, mut request: Request) -> Self::Future {
        let clone = self.inner.clone();
        let mut inner = mem::replace(&mut self.inner, clone);
        let api = self.api.clone();

        Box::pin(async move {
            if let Err(status) = authenticate_request(&api, &mut request).await {
                return Ok(status.into_response());
            }

            inner.call(request).await
        })
    }
}

impl From<sqlx::Error> for ApiKeyVerificationError {
    fn from(value: sqlx::Error) -> Self {
        Self::Database(value)
    }
}

async fn authenticate_request(api: &Api, request: &mut Request) -> Result<(), StatusCode> {
    let authorization = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let credentials = ApiKeyCredentials::from_authorization_header(authorization)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    let api_key = match verify_api_key(api, &credentials).await {
        Ok(api_key) => api_key,
        Err(ApiKeyVerificationError::InvalidCredentials) => {
            return Err(StatusCode::UNAUTHORIZED);
        }
        Err(ApiKeyVerificationError::Database(error)) => {
            tracing::error!(
                error = %error,
                "failed to verify API key"
            );
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    request.extensions_mut().insert(api_key);
    Ok(())
}

async fn verify_api_key(
    api: &Api,
    credentials: &ApiKeyCredentials,
) -> Result<ApiKey, ApiKeyVerificationError> {
    let api_key = sqlx::query_as::<_, ApiKeyRecord>(
        r#"
        SELECT name, public_id, secret_digest, created_at, expires_at
        FROM api_keys
        WHERE public_id = ?
        "#,
    )
    .bind(credentials.public_id())
    .fetch_optional(api.pool())
    .await?
    .ok_or(ApiKeyVerificationError::InvalidCredentials)?;

    if !credentials.matches_digest(&api_key.secret_digest) {
        return Err(ApiKeyVerificationError::InvalidCredentials);
    }

    if api_key
        .expires_at
        .is_some_and(|expires_at| expires_at <= Utc::now())
    {
        return Err(ApiKeyVerificationError::InvalidCredentials);
    }

    let scopes = sqlx::query_scalar::<_, String>(
        r#"
        SELECT scope
        FROM api_key_scopes
        WHERE api_key_public_id = ?
        "#,
    )
    .bind(&api_key.public_id)
    .fetch_all(api.pool())
    .await?;

    let expires_at = api_key.expires_at.map_or(Nullable::Null, Nullable::Present);

    Ok(ApiKey::new(
        api_key.name,
        api_key.public_id,
        scopes,
        api_key.created_at,
        expires_at,
    ))
}

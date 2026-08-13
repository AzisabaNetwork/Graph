use crate::auth::ApiKeyCredentials;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use axum_extra::TypedHeader;
use headers::Authorization;
use headers::authorization::Bearer;
use http::StatusCode;
use sqlx::MySqlPool;

pub(super) async fn authenticate(
    State(pool): State<MySqlPool>,
    TypedHeader(authorization): TypedHeader<Authorization<Bearer>>,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let credentials = authorization
        .token()
        .parse::<ApiKeyCredentials>()
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    let api_key = credentials
        .authenticate(&pool)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to authenticate MCP API key");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::UNAUTHORIZED)?;

    request.extensions_mut().insert(api_key);

    Ok(next.run(request).await)
}

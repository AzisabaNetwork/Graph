use crate::api::Api;
use crate::auth::ApiKeyCredentials;
use async_trait::async_trait;
use graph_api::apis::{ApiAuthBasic, BasicAuthKind};
use graph_api::models::ApiKey;
use http::{HeaderMap, header::AUTHORIZATION};

const BEARER_SCHEME: &str = "Bearer";

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

        let authorization = headers.get(AUTHORIZATION)?.to_str().ok()?;
        let mut parts = authorization.split_whitespace();
        let scheme = parts.next()?;
        let token = parts.next()?;

        if parts.next().is_some() || !scheme.eq_ignore_ascii_case(BEARER_SCHEME) {
            return None;
        }

        let credentials = token.parse::<ApiKeyCredentials>().ok()?;
        credentials
            .authenticate(self.pool())
            .await
            .unwrap_or_else(|error| {
                tracing::error!(%error, "failed to authenticate API key");
                None
            })
    }
}

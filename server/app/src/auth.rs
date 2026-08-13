use crate::records::ApiKeyRecord;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use graph_api::models::{ApiKey, ApiKeyScope};
use sha2::{Digest, Sha256};
use sqlx::MySqlPool;
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::str::FromStr;
use subtle::ConstantTimeEq;

const API_KEY_PREFIX: &str = "azk_live_";
const PUBLIC_ID_BYTES: usize = 12;
const SECRET_BYTES: usize = 32;

pub(crate) struct ApiKeyCredentials {
    public_id: String,
    secret: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApiKeyCredentialsParseError {
    Prefix,
    Format,
    PublicId,
    Secret,
}

pub(crate) trait ApiKeyScopeChecker {
    fn has_scope(&self, scope: &ApiKeyScope) -> bool;

    fn has_all_scopes(&self, scopes: &[ApiKeyScope]) -> bool;

    fn has_any_scope(&self, scopes: &[ApiKeyScope]) -> bool;
}

impl ApiKeyCredentials {
    pub(crate) fn generate() -> Result<Self, getrandom::Error> {
        Ok(Self {
            public_id: generate_component::<PUBLIC_ID_BYTES>()?,
            secret: generate_component::<SECRET_BYTES>()?,
        })
    }

    pub(crate) fn public_id(&self) -> &str {
        &self.public_id
    }

    pub(crate) fn secret_digest(&self) -> [u8; 32] {
        Sha256::digest(self.secret.as_bytes()).into()
    }

    pub(crate) fn matches_secret_digest(&self, expected: &[u8]) -> bool {
        let actual = self.secret_digest();

        expected.len() == actual.len() && bool::from(expected.ct_eq(&actual[..]))
    }

    pub(crate) fn expose(&self) -> String {
        format!("{API_KEY_PREFIX}{}.{}", self.public_id, self.secret)
    }

    pub(crate) async fn authenticate(
        &self,
        pool: &MySqlPool,
    ) -> Result<Option<ApiKey>, sqlx::Error> {
        let Some(record) = sqlx::query_as::<_, ApiKeyRecord>(
            r#"
            SELECT k.name, k.public_id, k.secret_digest, k.created_at, k.expires_at, p.player_id
            FROM api_keys k
            LEFT JOIN api_key_players p ON p.api_key_public_id = k.public_id
            WHERE k.public_id = ?
            "#,
        )
        .bind(&self.public_id)
        .fetch_optional(pool)
        .await?
        else {
            return Ok(None);
        };

        if !self.matches_secret_digest(&record.secret_digest)
            || record
                .expires_at
                .is_some_and(|expires_at| expires_at <= Utc::now())
        {
            return Ok(None);
        }

        let scopes = sqlx::query_scalar::<_, String>(
            r#"
            SELECT scope
            FROM api_key_scopes
            WHERE api_key_public_id = ?
            "#,
        )
        .bind(&record.public_id)
        .fetch_all(pool)
        .await?;

        Ok(Some(record.into_api_key(scopes)))
    }
}

impl FromStr for ApiKeyCredentials {
    type Err = ApiKeyCredentialsParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let value = s
            .strip_prefix(API_KEY_PREFIX)
            .ok_or(ApiKeyCredentialsParseError::Prefix)?;

        let (public_id, secret) = value
            .split_once('.')
            .ok_or(ApiKeyCredentialsParseError::Format)?;

        if !is_valid_component::<PUBLIC_ID_BYTES>(public_id) {
            return Err(ApiKeyCredentialsParseError::PublicId);
        }

        if !is_valid_component::<SECRET_BYTES>(secret) {
            return Err(ApiKeyCredentialsParseError::Secret);
        }

        Ok(Self {
            public_id: public_id.to_owned(),
            secret: secret.to_owned(),
        })
    }
}

impl Debug for ApiKeyCredentials {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeyCredentials")
            .field("public_id", &self.public_id)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

impl Display for ApiKeyCredentialsParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Prefix => "invalid API key prefix",
            Self::Format => "invalid API key format",
            Self::PublicId => "invalid API key public ID",
            Self::Secret => "invalid API key secret",
        })
    }
}

impl Error for ApiKeyCredentialsParseError {}

impl ApiKeyScopeChecker for ApiKey {
    fn has_scope(&self, scope: &ApiKeyScope) -> bool {
        let star = ApiKeyScope::Star.to_string();
        let scope = scope.to_string();

        self.scopes
            .iter()
            .any(|granted| granted == &star || granted == &scope)
    }

    fn has_all_scopes(&self, scopes: &[ApiKeyScope]) -> bool {
        scopes.iter().all(|scope| self.has_scope(scope))
    }

    fn has_any_scope(&self, scopes: &[ApiKeyScope]) -> bool {
        scopes.iter().any(|scope| self.has_scope(scope))
    }
}

fn generate_component<const N: usize>() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; N];

    getrandom::fill(&mut bytes)?;

    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn is_valid_component<const N: usize>(value: &str) -> bool {
    let Ok(decoded) = URL_SAFE_NO_PAD.decode(value) else {
        return false;
    };

    decoded.len() == N && URL_SAFE_NO_PAD.encode(decoded) == value
}

#[cfg(test)]
mod tests {
    use super::*;
    use graph_api::types::Nullable;

    #[test]
    fn generated_credentials_round_trip_without_exposing_the_secret_in_debug_output() {
        let credentials = ApiKeyCredentials::generate().unwrap();
        let token = credentials.expose();
        let parsed = token.parse::<ApiKeyCredentials>().unwrap();

        assert_eq!(parsed.public_id(), credentials.public_id());
        assert!(parsed.matches_secret_digest(&credentials.secret_digest()));
        assert!(!format!("{credentials:?}").contains(&credentials.secret));
    }

    #[test]
    fn rejects_non_canonical_credentials() {
        assert_eq!(
            "invalid".parse::<ApiKeyCredentials>().unwrap_err(),
            ApiKeyCredentialsParseError::Prefix
        );
    }

    #[test]
    fn star_grants_every_scope() {
        let api_key = api_key_with_scopes(&[ApiKeyScope::Star]);

        assert!(api_key.has_scope(&ApiKeyScope::ApiKeysColonWrite));
        assert!(api_key.has_all_scopes(&[
            ApiKeyScope::PlayersColonRead,
            ApiKeyScope::PatchNotesColonWrite,
        ]));
        assert!(api_key.has_any_scope(&[
            ApiKeyScope::CrawlsColonRead,
            ApiKeyScope::PunishmentsColonWrite,
        ]));
    }

    #[test]
    fn scopes_do_not_imply_other_scopes() {
        let api_key = api_key_with_scopes(&[ApiKeyScope::PlayersColonReadDetails]);

        assert!(api_key.has_scope(&ApiKeyScope::PlayersColonReadDetails));
        assert!(!api_key.has_scope(&ApiKeyScope::PlayersColonRead));
        assert!(!api_key.has_any_scope(&[ApiKeyScope::PlayersColonWrite]));
    }

    fn api_key_with_scopes(scopes: &[ApiKeyScope]) -> ApiKey {
        ApiKey::new(
            "Test API key".to_string(),
            "test-public-id".to_string(),
            Nullable::Null,
            scopes.iter().map(ToString::to_string).collect(),
            Utc::now(),
            Nullable::Null,
        )
    }
}

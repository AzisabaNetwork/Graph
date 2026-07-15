use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::str::FromStr;
use subtle::ConstantTimeEq;

const AUTHORIZATION_SCHEME: &str = "Bearer";
const API_KEY_PREFIX: &str = "azk_live_";
const PUBLIC_ID_BYTES: usize = 12;
const SECRET_BYTES: usize = 32;

pub(crate) struct ApiKeyCredentials {
    public_id: String,
    secret: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApiKeyParseError {
    InvalidAuthorizationHeader,
    UnsupportedAuthorizationScheme,
    InvalidPrefix,
    InvalidFormat,
    InvalidPublicId,
    InvalidSecret,
}

impl ApiKeyCredentials {
    pub(crate) fn generate() -> Result<Self, getrandom::Error> {
        Ok(Self {
            public_id: generate_token_component::<PUBLIC_ID_BYTES>()?,
            secret: generate_token_component::<SECRET_BYTES>()?,
        })
    }

    pub(crate) fn from_authorization_header(header: &str) -> Result<Self, ApiKeyParseError> {
        let mut parts = header.split_whitespace();

        let scheme = parts
            .next()
            .ok_or(ApiKeyParseError::InvalidAuthorizationHeader)?;

        let token = parts
            .next()
            .ok_or(ApiKeyParseError::InvalidAuthorizationHeader)?;

        if parts.next().is_some() {
            return Err(ApiKeyParseError::InvalidAuthorizationHeader);
        }

        if !scheme.eq_ignore_ascii_case(AUTHORIZATION_SCHEME) {
            return Err(ApiKeyParseError::UnsupportedAuthorizationScheme);
        }

        token.parse()
    }

    pub(crate) fn public_id(&self) -> &str {
        &self.public_id
    }

    pub(crate) fn to_token(&self) -> String {
        format!("{API_KEY_PREFIX}{}.{}", self.public_id, self.secret)
    }

    pub(crate) fn secret_digest(&self) -> [u8; 32] {
        Sha256::digest(self.secret.as_bytes()).into()
    }

    pub(crate) fn matches_digest(&self, expected_digest: &[u8]) -> bool {
        let actual_digest = self.secret_digest();

        expected_digest.len() == actual_digest.len()
            && bool::from(expected_digest.ct_eq(&actual_digest[..]))
    }
}

impl FromStr for ApiKeyCredentials {
    type Err = ApiKeyParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let value = s
            .strip_prefix(API_KEY_PREFIX)
            .ok_or(ApiKeyParseError::InvalidPrefix)?;

        let (public_id, secret) = value
            .split_once('.')
            .ok_or(ApiKeyParseError::InvalidFormat)?;

        if !is_valid_token_component::<PUBLIC_ID_BYTES>(public_id) {
            return Err(ApiKeyParseError::InvalidPublicId);
        }

        if !is_valid_token_component::<SECRET_BYTES>(secret) {
            return Err(ApiKeyParseError::InvalidSecret);
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

impl Display for ApiKeyParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            ApiKeyParseError::InvalidAuthorizationHeader => "invalid authorization header",
            ApiKeyParseError::UnsupportedAuthorizationScheme => "unsupported authorization scheme",
            ApiKeyParseError::InvalidPrefix => "invalid API key prefix",
            ApiKeyParseError::InvalidFormat => "invalid API key format",
            ApiKeyParseError::InvalidPublicId => "invalid API key public ID",
            ApiKeyParseError::InvalidSecret => "invalid API key secret",
        };

        f.write_str(message)
    }
}

impl Error for ApiKeyParseError {}

fn generate_token_component<const N: usize>() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; N];

    getrandom::fill(&mut bytes)?;

    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn is_valid_token_component<const N: usize>(value: &str) -> bool {
    let Ok(decoded) = URL_SAFE_NO_PAD.decode(value) else {
        return false;
    };

    decoded.len() == N && URL_SAFE_NO_PAD.encode(decoded) == value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_credentials_round_trip() {
        let credentials = ApiKeyCredentials::generate().unwrap();
        let token = credentials.to_token();
        let parsed = token.parse::<ApiKeyCredentials>().unwrap();

        assert_eq!(parsed.public_id, credentials.public_id);
        assert!(parsed.matches_digest(&credentials.secret_digest()));
        assert!(!format!("{credentials:?}").contains(&credentials.secret));
    }

    #[test]
    fn authorization_header_requires_bearer_scheme() {
        let credentials = ApiKeyCredentials::generate().unwrap();
        let token = credentials.to_token();

        assert!(ApiKeyCredentials::from_authorization_header(&format!("Bearer {token}")).is_ok());
        assert_eq!(
            ApiKeyCredentials::from_authorization_header(&format!("ApiKey {token}")).unwrap_err(),
            ApiKeyParseError::UnsupportedAuthorizationScheme
        );
    }
}

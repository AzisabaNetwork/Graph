use reqwest::{Client, StatusCode};
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, Semaphore};
use uuid::Uuid;

const SESSION_SERVER_BASE_URL: &str = "https://sessionserver.mojang.com";
const PROFILE_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const NOT_FOUND_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_CONCURRENT_REQUESTS: usize = 4;

#[derive(Clone, Debug)]
pub(crate) struct MojangClient {
    inner: Arc<MojangClientInner>,
}

#[derive(Debug)]
struct MojangClientInner {
    client: Client,
    base_url: String,
    cache: RwLock<HashMap<Uuid, CacheEntry>>,
    profile_locks: Mutex<HashMap<Uuid, Arc<Mutex<()>>>>,
    request_slots: Semaphore,
    profile_cache_ttl: Duration,
    not_found_cache_ttl: Duration,
}

#[derive(Clone, Debug)]
struct CacheEntry {
    username: Option<String>,
    expires_at: Instant,
}

#[derive(Debug)]
pub(crate) enum MojangError {
    Client(reqwest::Error),
    UnexpectedStatus(StatusCode),
    InvalidProfileId(String),
    ProfileIdMismatch { requested: Uuid, returned: Uuid },
    RequestLimitClosed,
}

#[derive(Debug, Deserialize)]
struct SessionProfile {
    id: String,
    name: String,
}

impl MojangClient {
    pub(crate) fn new() -> Result<Self, MojangError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .user_agent("Azisaba-Graph/0.0.1")
            .build()
            .map_err(MojangError::Client)?;

        Ok(Self::with_client(
            client,
            SESSION_SERVER_BASE_URL.to_string(),
            PROFILE_CACHE_TTL,
            NOT_FOUND_CACHE_TTL,
        ))
    }

    pub(crate) async fn username(&self, player_id: Uuid) -> Result<Option<String>, MojangError> {
        if let Some(username) = self.cached_username(player_id).await {
            return Ok(username);
        }

        let profile_lock = {
            let mut locks = self.inner.profile_locks.lock().await;
            locks
                .entry(player_id)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _profile_guard = profile_lock.lock().await;

        if let Some(username) = self.cached_username(player_id).await {
            return Ok(username);
        }

        let username = self.fetch_username(player_id).await?;
        let ttl = if username.is_some() {
            self.inner.profile_cache_ttl
        } else {
            self.inner.not_found_cache_ttl
        };
        self.inner.cache.write().await.insert(
            player_id,
            CacheEntry {
                username: username.clone(),
                expires_at: Instant::now() + ttl,
            },
        );

        Ok(username)
    }

    async fn cached_username(&self, player_id: Uuid) -> Option<Option<String>> {
        let cache = self.inner.cache.read().await;
        cache
            .get(&player_id)
            .filter(|entry| entry.expires_at > Instant::now())
            .map(|entry| entry.username.clone())
    }

    async fn fetch_username(&self, player_id: Uuid) -> Result<Option<String>, MojangError> {
        let _request_slot = self
            .inner
            .request_slots
            .acquire()
            .await
            .map_err(|_| MojangError::RequestLimitClosed)?;
        let url = format!(
            "{}/session/minecraft/profile/{}",
            self.inner.base_url,
            player_id.simple()
        );
        let response = self
            .inner
            .client
            .get(url)
            .send()
            .await
            .map_err(MojangError::Client)?;

        match response.status() {
            StatusCode::OK => {
                let profile = response
                    .json::<SessionProfile>()
                    .await
                    .map_err(MojangError::Client)?;
                let returned_id = Uuid::parse_str(&profile.id)
                    .map_err(|_| MojangError::InvalidProfileId(profile.id.clone()))?;
                if returned_id != player_id {
                    return Err(MojangError::ProfileIdMismatch {
                        requested: player_id,
                        returned: returned_id,
                    });
                }
                Ok(Some(profile.name))
            }
            StatusCode::NO_CONTENT | StatusCode::NOT_FOUND => Ok(None),
            status => Err(MojangError::UnexpectedStatus(status)),
        }
    }

    fn with_client(
        client: Client,
        base_url: String,
        profile_cache_ttl: Duration,
        not_found_cache_ttl: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(MojangClientInner {
                client,
                base_url: base_url.trim_end_matches('/').to_string(),
                cache: RwLock::new(HashMap::new()),
                profile_locks: Mutex::new(HashMap::new()),
                request_slots: Semaphore::new(MAX_CONCURRENT_REQUESTS),
                profile_cache_ttl,
                not_found_cache_ttl,
            }),
        }
    }
}

impl fmt::Display for MojangError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Client(error) => write!(formatter, "Mojang API request failed: {error}"),
            Self::UnexpectedStatus(status) => {
                write!(formatter, "Mojang API returned unexpected status {status}")
            }
            Self::InvalidProfileId(id) => {
                write!(formatter, "Mojang API returned invalid profile ID `{id}`")
            }
            Self::ProfileIdMismatch {
                requested,
                returned,
            } => write!(
                formatter,
                "Mojang API returned profile ID {returned} for requested ID {requested}"
            ),
            Self::RequestLimitClosed => write!(formatter, "Mojang API request limiter is closed"),
        }
    }
}

impl std::error::Error for MojangError {}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::Router;
    use axum::routing::get;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn caches_successful_profile_lookups() {
        let player_id = Uuid::parse_str("8667ba71-b85a-4004-af54-457a9734eed7").unwrap();
        let request_count = Arc::new(AtomicUsize::new(0));
        let handler_count = request_count.clone();
        let app = Router::new().route(
            "/session/minecraft/profile/:id",
            get(move || {
                let handler_count = handler_count.clone();
                async move {
                    handler_count.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "id": "8667ba71b85a4004af54457a9734eed7",
                        "name": "Steve"
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = MojangClient::with_client(
            Client::new(),
            format!("http://{address}"),
            Duration::from_secs(60),
            Duration::from_secs(60),
        );

        assert_eq!(
            client.username(player_id).await.unwrap().as_deref(),
            Some("Steve")
        );
        assert_eq!(
            client.username(player_id).await.unwrap().as_deref(),
            Some("Steve")
        );
        assert_eq!(request_count.load(Ordering::SeqCst), 1);

        server.abort();
    }

    #[tokio::test]
    async fn caches_missing_profiles() {
        let player_id = Uuid::nil();
        let request_count = Arc::new(AtomicUsize::new(0));
        let handler_count = request_count.clone();
        let app = Router::new().route(
            "/session/minecraft/profile/:id",
            get(move || {
                let handler_count = handler_count.clone();
                async move {
                    handler_count.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NO_CONTENT
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = MojangClient::with_client(
            Client::new(),
            format!("http://{address}"),
            Duration::from_secs(60),
            Duration::from_secs(60),
        );

        assert_eq!(client.username(player_id).await.unwrap(), None);
        assert_eq!(client.username(player_id).await.unwrap(), None);
        assert_eq!(request_count.load(Ordering::SeqCst), 1);

        server.abort();
    }
}

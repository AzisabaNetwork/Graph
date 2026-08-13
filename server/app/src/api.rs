pub(crate) mod api_keys;
pub(crate) mod auth;
pub(crate) mod crawls;
pub(crate) mod filters;
pub(crate) mod pagination;
pub(crate) mod patch_notes;
pub(crate) mod players;
pub(crate) mod punishments;
pub(crate) mod stream;

use crate::mojang::MojangProfileResolver;
use crate::object_storage::ObjectStorage;
use redis::aio::ConnectionManager;
use sqlx::MySqlPool;
use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub(crate) struct Api {
    default_pool: MySqlPool,
    punishments_pool: MySqlPool,
    object_storage: Option<ObjectStorage>,
    profile_resolver: MojangProfileResolver,
    redis_publisher: Option<ConnectionManager>,
    stream_events: broadcast::Sender<graph_api::models::StreamEvent>,
}

impl Api {
    pub(crate) fn new(
        pool: MySqlPool,
        punishments_pool: MySqlPool,
        object_storage: Option<ObjectStorage>,
        profile_resolver: MojangProfileResolver,
    ) -> Self {
        let (stream_events, _) = broadcast::channel(256);
        Self {
            default_pool: pool,
            punishments_pool,
            object_storage,
            profile_resolver,
            redis_publisher: None,
            stream_events,
        }
    }

    pub(crate) fn new_with_redis(
        pool: MySqlPool,
        punishments_pool: MySqlPool,
        object_storage: Option<ObjectStorage>,
        profile_resolver: MojangProfileResolver,
        redis_client: redis::Client,
        redis: ConnectionManager,
    ) -> Self {
        let mut api = Self::new(pool, punishments_pool, object_storage, profile_resolver);
        api.redis_publisher = Some(redis);
        api.start_stream_event_listener(redis_client);
        api
    }

    pub(crate) fn default_pool(&self) -> &MySqlPool {
        &self.default_pool
    }

    pub(crate) fn punishments_pool(&self) -> &MySqlPool {
        &self.punishments_pool
    }

    pub(crate) fn profile_resolver(&self) -> &MojangProfileResolver {
        &self.profile_resolver
    }
}

#[async_trait::async_trait]
impl graph_api::apis::ErrorHandler<String> for Api {}

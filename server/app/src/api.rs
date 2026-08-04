mod api_keys;
mod crawls;
mod patch_notes;
mod players;
mod punishments;

use crate::mojang::PlayerDbClient;
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::Client as S3Client;
use aws_sdk_s3::config::Region;
use graph_api::types::Nullable;
use sqlx::MySqlPool;
use std::env;

#[derive(Clone, Debug)]
pub(crate) struct Api {
    pool: MySqlPool,
    punishments_pool: MySqlPool,
    object_storage: Option<ObjectStorage>,
    player_db: PlayerDbClient,
}

impl Api {
    pub(crate) fn new(
        pool: MySqlPool,
        punishments_pool: MySqlPool,
        object_storage: Option<ObjectStorage>,
        player_db: PlayerDbClient,
    ) -> Self {
        Self {
            pool,
            punishments_pool,
            object_storage,
            player_db,
        }
    }

    pub(crate) fn pool(&self) -> &MySqlPool {
        &self.pool
    }

    pub(crate) fn punishments_pool(&self) -> &MySqlPool {
        &self.punishments_pool
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ObjectStorage {
    client: S3Client,
    bucket: String,
    public_url_base: String,
}

impl ObjectStorage {
    pub(crate) fn new(client: S3Client, bucket: String, public_url_base: String) -> Self {
        Self {
            client,
            bucket,
            public_url_base: public_url_base.trim_end_matches('/').to_string(),
        }
    }

    pub(crate) async fn from_env() -> Option<Self> {
        let bucket = non_empty_env("R2_BUCKET");
        let public_url_base = non_empty_env("R2_PUBLIC_URL_BASE");
        let access_key_id = non_empty_env("R2_ACCESS_KEY_ID");
        let secret_access_key = non_empty_env("R2_SECRET_ACCESS_KEY");
        let endpoint_url = non_empty_env("R2_ENDPOINT_URL").or_else(|| {
            non_empty_env("R2_ACCOUNT_ID")
                .map(|account_id| format!("https://{account_id}.r2.cloudflarestorage.com"))
        });

        let (
            Some(bucket),
            Some(public_url_base),
            Some(access_key_id),
            Some(secret_access_key),
            Some(endpoint_url),
        ) = (
            bucket,
            public_url_base,
            access_key_id,
            secret_access_key,
            endpoint_url,
        )
        else {
            tracing::warn!("R2 object storage is not configured; file uploads will fail");
            return None;
        };

        let sdk_config = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new("auto"))
            .endpoint_url(endpoint_url)
            .credentials_provider(Credentials::new(
                access_key_id,
                secret_access_key,
                None,
                None,
                "r2-env",
            ))
            .load()
            .await;

        Some(Self::new(
            S3Client::new(&sdk_config),
            bucket,
            public_url_base,
        ))
    }

    fn public_url(&self, object_key: &str) -> String {
        format!("{}/{}", self.public_url_base, object_key)
    }
}

pub(crate) fn non_empty_env(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.trim().is_empty())
}

pub(crate) fn into_nullable<T>(value: Option<T>) -> Nullable<T> {
    value.map_or(Nullable::Null, Nullable::Present)
}

pub(crate) fn from_nullable<T>(value: &Nullable<T>) -> Option<&T> {
    match value {
        Nullable::Present(value) => Some(value),
        Nullable::Null => None,
    }
}

#[async_trait::async_trait]
impl graph_api::apis::ErrorHandler<String> for Api {}

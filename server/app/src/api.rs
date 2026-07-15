mod api_keys;
mod patch_notes;

use aws_sdk_s3::Client as S3Client;
use sqlx::MySqlPool;

#[derive(Clone, Debug)]
pub(crate) struct Api {
    pool: MySqlPool,
    image_storage: Option<ImageStorage>,
}

#[derive(Clone, Debug)]
pub(crate) struct ImageStorage {
    client: S3Client,
    bucket: String,
    public_url_base: String,
}

impl Api {
    pub(crate) fn new(pool: MySqlPool, image_storage: Option<ImageStorage>) -> Self {
        Self {
            pool,
            image_storage,
        }
    }

    pub(crate) fn pool(&self) -> &MySqlPool {
        &self.pool
    }
}

impl ImageStorage {
    pub(crate) fn new(client: S3Client, bucket: String, public_url_base: String) -> Self {
        Self {
            client,
            bucket,
            public_url_base: public_url_base.trim_end_matches('/').to_string(),
        }
    }

    fn public_url(&self, object_key: &str) -> String {
        format!("{}/{}", self.public_url_base, object_key)
    }
}

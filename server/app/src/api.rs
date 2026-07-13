use aws_sdk_s3::Client as S3Client;
use sqlx::MySqlPool;

pub mod patch_notes;

#[derive(Clone, Debug)]
pub struct Api {
    pub(crate) pool: MySqlPool,
    pub(crate) image_storage: Option<ImageStorage>,
}

impl Api {
    pub fn new(pool: MySqlPool, image_storage: Option<ImageStorage>) -> Self {
        Self {
            pool,
            image_storage,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ImageStorage {
    pub(crate) client: S3Client,
    pub(crate) bucket: String,
    pub(crate) public_url_base: String,
}

impl ImageStorage {
    pub fn new(client: S3Client, bucket: String, public_url_base: String) -> Self {
        Self {
            client,
            bucket,
            public_url_base: public_url_base.trim_end_matches('/').to_string(),
        }
    }

    pub(crate) fn public_url(&self, object_key: &str) -> String {
        format!("{}/{}", self.public_url_base, object_key)
    }
}

use crate::non_empty_env;
use aws_config::{BehaviorVersion, Region};
use aws_credential_types::Credentials;
use aws_sdk_s3::Client;

#[derive(Clone, Debug)]
pub(crate) struct ObjectStorage {
    pub(crate) client: Client,
    pub(crate) bucket: String,
    pub(crate) public_url_base: String,
}

impl ObjectStorage {
    pub(crate) fn new(client: Client, bucket: String, public_url_base: String) -> Self {
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

        Some(Self::new(Client::new(&sdk_config), bucket, public_url_base))
    }

    pub(crate) fn build_public_url(&self, object_key: &str) -> String {
        format!("{}/{}", self.public_url_base, object_key)
    }
}

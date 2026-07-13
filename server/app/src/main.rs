mod api;
mod pagination;

use api::{Api, ImageStorage};
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::{Client as S3Client, config::Region};
use sqlx::MySqlPool;
use std::{env, net::SocketAddr, sync::Arc};
use tower_http::trace::TraceLayer;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "graph_server=info,tower_http=info".into()),
        )
        .init();

    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = MySqlPool::connect(&database_url)
        .await
        .expect("failed to connect database");

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("failed to run database migrations");

    let image_storage = build_image_storage().await;

    let addr = env::var("GRAPH_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".to_string());
    let addr: SocketAddr = addr
        .parse()
        .expect("GRAPH_SERVER_ADDR must be a valid socket address");

    let app = graph_api::server::new(Arc::new(Api::new(pool, image_storage)))
        .layer(TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind server socket");

    tracing::info!(%addr, "graph-server listening");
    axum::serve(listener, app)
        .await
        .expect("graph-server failed");
}

async fn build_image_storage() -> Option<ImageStorage> {
    let bucket = optional_env("R2_BUCKET");
    let public_url_base = optional_env("R2_PUBLIC_URL_BASE");
    let access_key_id = optional_env("R2_ACCESS_KEY_ID");
    let secret_access_key = optional_env("R2_SECRET_ACCESS_KEY");
    let endpoint_url = optional_env("R2_ENDPOINT_URL").or_else(|| {
        optional_env("R2_ACCOUNT_ID")
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
        tracing::warn!("R2 image storage is not configured; image uploads will fail");
        return None;
    };

    let config = aws_config::defaults(BehaviorVersion::latest())
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

    Some(ImageStorage::new(
        S3Client::new(&config),
        bucket,
        public_url_base,
    ))
}

fn optional_env(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.trim().is_empty())
}

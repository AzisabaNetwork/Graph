mod api;
mod auth;
mod mcp;
mod mojang;
mod object_storage;
mod records;

use crate::object_storage::ObjectStorage;
use api::Api;
use mojang::MojangProfileResolver;
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
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

    let punishments_database_url =
        env::var("PUNISHMENTS_DATABASE_URL").expect("PUNISHMENTS_DATABASE_URL must be set");
    let punishments_pool = MySqlPool::connect(&punishments_database_url)
        .await
        .expect("failed to connect punishments database");

    validate_punishments_database(&punishments_pool)
        .await
        .expect("punishments database schema is incompatible");

    let redis_url = env::var("REDIS_URL").expect("REDIS_URL must be set");
    let redis_client = redis::Client::open(redis_url).expect("REDIS_URL must be valid");

    let redis_config = ConnectionManagerConfig::new()
        .set_min_delay(std::time::Duration::from_millis(250))
        .set_max_delay(std::time::Duration::from_secs(5))
        .set_number_of_retries(5)
        .set_connection_timeout(Some(std::time::Duration::from_secs(5)))
        .set_response_timeout(Some(std::time::Duration::from_secs(5)));

    let redis = ConnectionManager::new_lazy_with_config(redis_client.clone(), redis_config)
        .expect("failed to configure Redis connection manager");

    let api = Api::new_with_redis(
        pool.clone(),
        punishments_pool,
        ObjectStorage::from_env().await,
        MojangProfileResolver::new().expect("failed to create PlayerDB client"),
        redis_client,
        redis,
    );

    if let Some(bootstrap_api_key) = non_empty_env("GRAPH_BOOTSTRAP_API_KEY") {
        api.provision_bootstrap_api_key(&bootstrap_api_key)
            .await
            .expect("failed to provision bootstrap API key");

        tracing::info!("bootstrap API key provisioned");
    }

    let addr = env::var("GRAPH_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".to_string());
    let addr: SocketAddr = addr
        .parse()
        .expect("GRAPH_SERVER_ADDR must be a valid socket address");

    let api = Arc::new(api);

    let app = graph_api::server::new(api.clone())
        .merge(mcp::router(
            pool.clone(),
            api.punishments_pool().clone(),
            api.profile_resolver().clone(),
        ))
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind server socket");

    tracing::info!(%addr, "graph-server listening");

    axum::serve(listener, app)
        .await
        .expect("graph-server failed");
}

pub(crate) fn non_empty_env(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.trim().is_empty())
}

async fn validate_punishments_database(pool: &MySqlPool) -> Result<(), sqlx::Error> {
    for query in [
        "SELECT 1 FROM `punishmentHistory` LIMIT 1",
        "SELECT 1 FROM `punishments` LIMIT 1",
        "SELECT 1 FROM `unpunish` LIMIT 1",
        "SELECT 1 FROM `proofs` LIMIT 1",
        "SELECT 1 FROM `events` LIMIT 1",
    ] {
        sqlx::query(query).fetch_optional(pool).await?;
    }

    Ok(())
}

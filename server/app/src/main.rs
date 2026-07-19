mod api;
mod auth;
mod mojang;
mod pagination;

use crate::api::non_empty_env;
use api::{Api, ObjectStorage};
use mojang::PlayerDbClient;
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

    let api = Api::new(
        pool,
        ObjectStorage::from_env().await,
        PlayerDbClient::new().expect("failed to create PlayerDB client"),
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

    let app = graph_api::server::new(Arc::new(api)).layer(TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind server socket");

    tracing::info!(%addr, "graph-server listening");
    axum::serve(listener, app)
        .await
        .expect("graph-server failed");
}

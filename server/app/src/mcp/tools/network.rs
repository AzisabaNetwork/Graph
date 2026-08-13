use crate::auth::ApiKeyScopeChecker;
use crate::mcp::Mcp;
use crate::mcp::models::{NetworkOverview, PopulationPoint, PopulationTrend, ServerStatus};
use chrono::{DateTime, Duration, Utc};
use graph_api::models::ApiKeyScope;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
pub(super) struct PopulationTrendArgs {
    pub address: String,
    pub port: u16,
    /// Range to query (e.g., "24h", "7d", "30d")
    pub range: String,
    /// Aggregation interval (e.g., "1h", "6h", "12h", "1d")
    pub interval: String,
}

#[tool_router(router = network_tools, vis = "pub(super)")]
impl Mcp {
    #[tool(
        name = "get_population_trend",
        description = "Get aggregated population trends for a specific server."
    )]
    pub(super) async fn get_population_trend(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<PopulationTrendArgs>,
    ) -> Result<Json<PopulationTrend>, ErrorData> {
        let api_key = self.get_api_key(&ctx)?;
        let args = params.0;
        if !api_key.has_scope(&ApiKeyScope::CrawlsColonRead) {
            return Err(ErrorData::invalid_request(
                "missing required scope: crawls:read",
                None,
            ));
        }

        let duration = match args.range.as_str() {
            "24h" => Duration::hours(24),
            "7d" => Duration::days(7),
            "30d" => Duration::days(30),
            _ => {
                return Err(ErrorData::invalid_params(
                    "invalid range: must be 24h, 7d, or 30d",
                    None,
                ));
            }
        };

        let interval_secs = match args.interval.as_str() {
            "1h" => 3600,
            "6h" => 3600 * 6,
            "12h" => 3600 * 12,
            "1d" => 3600 * 24,
            _ => {
                return Err(ErrorData::invalid_params(
                    "invalid interval: must be 1h, 6h, 12h, or 1d",
                    None,
                ));
            }
        };

        let start_time = Utc::now() - duration;

        let rows = sqlx::query(
            r#"
            SELECT 
                FROM_UNIXTIME(FLOOR(UNIX_TIMESTAMP(crawled_at) / ?) * ?) as bucket,
                AVG(online_players) as avg_online,
                MAX(online_players) as max_online,
                COUNT(*) as sample_count
            FROM crawls
            WHERE address = ? AND port = ? AND crawled_at >= ?
            GROUP BY bucket
            ORDER BY bucket ASC
            LIMIT 500
            "#,
        )
        .bind(interval_secs)
        .bind(interval_secs)
        .bind(&args.address)
        .bind(args.port)
        .bind(start_time)
        .fetch_all(&self.default_pool)
        .await
        .map_err(|error| {
            tracing::error!(%error, address = %args.address, "failed to fetch population trend");
            ErrorData::internal_error("failed to fetch population trend", None)
        })?;

        let mut points = Vec::new();
        for row in rows {
            use sqlx::Row;
            let timestamp: Option<DateTime<Utc>> = row.try_get("bucket").ok();
            if let Some(timestamp) = timestamp {
                let avg_online = row.try_get::<f64, _>("avg_online").unwrap_or(0.0);
                let max_online = row.try_get::<u32, _>("max_online").unwrap_or(0);
                let sample_count = row.try_get::<i64, _>("sample_count").unwrap_or(0);

                points.push(PopulationPoint {
                    timestamp,
                    avg_online,
                    max_online,
                    sample_count: sample_count as u64,
                });
            }
        }

        let trend = PopulationTrend {
            address: args.address,
            port: args.port,
            from: start_time,
            to: Utc::now(),
            interval: args.interval,
            points,
        };

        Ok(Json(trend))
    }

    #[tool(
        name = "get_network_status",
        description = "Get the latest status snapshot for all monitored servers."
    )]
    pub(super) async fn get_network_status(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<NetworkOverview>, ErrorData> {
        let api_key = self.get_api_key(&ctx)?;
        if !api_key.has_scope(&ApiKeyScope::CrawlsColonRead) {
            return Err(ErrorData::invalid_request(
                "missing required scope: crawls:read",
                None,
            ));
        }

        let rows = sqlx::query(
            r#"
            SELECT address, port, online_players, max_players, version, crawled_at
            FROM crawls c1
            WHERE crawled_at = (
                SELECT MAX(crawled_at)
                FROM crawls c2
                WHERE c2.address = c1.address AND c2.port = c1.port
            )
            "#,
        )
        .fetch_all(&self.default_pool)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to fetch network status");
            ErrorData::internal_error("failed to fetch network status", None)
        })?;

        let mut servers = Vec::new();
        for row in rows {
            use sqlx::Row;
            servers.push(ServerStatus {
                address: row.get("address"),
                port: row.get("port"),
                online_players: row.get("online_players"),
                max_players: row.get("max_players"),
                version: row.get("version"),
                crawled_at: row.get("crawled_at"),
            });
        }

        let overview = NetworkOverview { servers };

        Ok(Json(overview))
    }
}

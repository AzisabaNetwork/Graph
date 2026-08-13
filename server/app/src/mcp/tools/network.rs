use crate::auth::ApiKeyScopeChecker;
use crate::mcp::Mcp;
use crate::mcp::models::{NetworkOverview, PopulationPoint, PopulationTrend, ServerStatus};
use chrono::{DateTime, Duration, Utc};
use graph_api::models::{ApiKey, ApiKeyScope};
use rmcp::ErrorData;
use rmcp::model::{CallToolResult, ContentBlock, TextContent, Tool};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize, JsonSchema)]
pub(super) struct PopulationTrendArgs {
    pub address: String,
    pub port: u16,
    /// Range to query (e.g., "24h", "7d", "30d")
    pub range: String,
    /// Aggregation interval (e.g., "1h", "6h", "12h", "1d")
    pub interval: String,
}

pub(super) fn tools() -> Vec<Tool> {
    let trend_schema = serde_json::to_value(schemars::schema_for!(PopulationTrendArgs))
        .unwrap()
        .as_object()
        .unwrap()
        .clone();

    vec![
        Tool::new(
            "get_population_trend",
            "Get aggregated population trends for a specific server.",
            Arc::new(trend_schema),
        ),
        Tool::new(
            "get_network_status",
            "Get the latest status snapshot for all monitored servers.",
            Arc::new(serde_json::Map::new()),
        ),
    ]
}

impl Mcp {
    pub(super) async fn get_population_trend(
        &self,
        api_key: &ApiKey,
        args: PopulationTrendArgs,
    ) -> Result<CallToolResult, ErrorData> {
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

        let content = serde_json::to_string(&trend).map_err(|error| {
            tracing::error!(%error, "failed to serialize population trend");
            ErrorData::internal_error("failed to serialize population trend", None)
        })?;

        Ok(CallToolResult::success(vec![ContentBlock::Text(
            TextContent::new(content),
        )]))
    }

    pub(super) async fn get_network_status(
        &self,
        api_key: &ApiKey,
    ) -> Result<CallToolResult, ErrorData> {
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

        let content = serde_json::to_string(&overview).map_err(|error| {
            tracing::error!(%error, "failed to serialize network status");
            ErrorData::internal_error("failed to serialize network status", None)
        })?;

        Ok(CallToolResult::success(vec![ContentBlock::Text(
            TextContent::new(content),
        )]))
    }
}

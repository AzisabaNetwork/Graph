use crate::api::Api;
use crate::api::filters::is_valid_half_open_range;
use crate::api::pagination::Cursor;
use crate::api::stream::{crawl_created_event, crawl_deleted_event};
use crate::auth::ApiKeyScopeChecker;
use crate::records::CrawlRecord;
use async_trait::async_trait;
use axum_extra::extract::CookieJar;
use chrono::{DateTime, Utc};
use graph_api::apis::crawls::{
    Crawls, CreateCrawlResponse, DeleteCrawlByIdResponse, GetCrawlByIdResponse, ListCrawlsResponse,
};
use graph_api::models::{
    ApiKey, ApiKeyScope, Crawl, CreateCrawlRequest, DeleteCrawlByIdPathParams,
    GetCrawlByIdPathParams, ListCrawls200Response, ListCrawlsQueryParams,
};
use graph_api::types::Nullable;
use headers::Host;
use http::Method;
use sqlx::{MySql, QueryBuilder};
use uuid::Uuid;

const DEFAULT_CRAWLS_LIMIT: u8 = 20;
const MAX_CRAWLS_LIMIT: u8 = 100;

type CrawlCursor = Cursor<DateTime<Utc>, Uuid>;

#[async_trait]
impl Crawls<String> for Api {
    type Claims = ApiKey;

    async fn create_crawl(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        body: &CreateCrawlRequest,
    ) -> Result<CreateCrawlResponse, String> {
        if !api_key.has_scope(&ApiKeyScope::CrawlsColonWrite) {
            return Ok(CreateCrawlResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope);
        }

        if !(1..=255).contains(&body.address.chars().count())
            || body.port == 0
            || !valid_favicon(&body.favicon)
        {
            return Ok(CreateCrawlResponse::Status400_TheRequestIsInvalid);
        }

        let id = Uuid::new_v4();
        let favicon = match &body.favicon {
            Nullable::Present(favicon) => Some(favicon),
            Nullable::Null => None,
        };
        sqlx::query(
            r#"
            INSERT INTO crawls
                (id, address, port, ping, version, protocol_version, max_players,
                 online_players, description, favicon, crawled_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(id)
        .bind(&body.address)
        .bind(body.port)
        .bind(body.ping)
        .bind(&body.version)
        .bind(body.protocol_version)
        .bind(body.max_players)
        .bind(body.online_players)
        .bind(&body.description)
        .bind(favicon)
        .bind(body.crawled_at)
        .execute(&self.default_pool)
        .await
        .map_err(log_database_error)?;

        let mut crawl = Crawl::new(
            id,
            body.address.clone(),
            body.port,
            body.ping,
            body.version.clone(),
            body.protocol_version,
            body.max_players,
            body.online_players,
            body.favicon.clone(),
            body.crawled_at,
        );
        crawl.description = body.description.clone();

        self.publish_stream_event(crawl_created_event(crawl.clone()))
            .await;

        Ok(CreateCrawlResponse::Status201_TheCrawlWasCreatedSuccessfully(crawl))
    }

    async fn delete_crawl_by_id(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        path_params: &DeleteCrawlByIdPathParams,
    ) -> Result<DeleteCrawlByIdResponse, String> {
        if !api_key.has_scope(&ApiKeyScope::CrawlsColonWrite) {
            return Ok(
                DeleteCrawlByIdResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope,
            );
        }

        let crawl = sqlx::query_as::<_, CrawlRecord>(
            r#"
            SELECT id, address, port, ping, version, protocol_version, max_players,
                   online_players, description, favicon, crawled_at
            FROM crawls
            WHERE id = ?
            "#,
        )
        .bind(path_params.crawl_id)
        .fetch_optional(&self.default_pool)
        .await
        .map_err(log_database_error)?;
        let Some(crawl) = crawl else {
            return Ok(DeleteCrawlByIdResponse::Status404_TheCrawlWasNotFound);
        };

        let result = sqlx::query("DELETE FROM crawls WHERE id = ?")
            .bind(path_params.crawl_id)
            .execute(&self.default_pool)
            .await
            .map_err(log_database_error)?;

        if result.rows_affected() == 0 {
            return Ok(DeleteCrawlByIdResponse::Status404_TheCrawlWasNotFound);
        }

        self.publish_stream_event(crawl_deleted_event(crawl.into()))
            .await;

        Ok(DeleteCrawlByIdResponse::Status204_TheCrawlWasDeletedSuccessfully)
    }

    async fn get_crawl_by_id(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        path_params: &GetCrawlByIdPathParams,
    ) -> Result<GetCrawlByIdResponse, String> {
        if !api_key.has_scope(&ApiKeyScope::CrawlsColonRead) {
            return Ok(GetCrawlByIdResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope);
        }

        let record = sqlx::query_as::<_, CrawlRecord>(
            r#"
            SELECT id, address, port, ping, version, protocol_version, max_players,
                   online_players, description, favicon, crawled_at
            FROM crawls
            WHERE id = ?
            "#,
        )
        .bind(path_params.crawl_id)
        .fetch_optional(&self.default_pool)
        .await
        .map_err(log_database_error)?;

        let Some(record) = record else {
            return Ok(GetCrawlByIdResponse::Status404_TheCrawlWasNotFound);
        };

        Ok(GetCrawlByIdResponse::Status200_TheCrawlWasRetrievedSuccessfully(record.into()))
    }

    async fn list_crawls(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
        query_params: &ListCrawlsQueryParams,
    ) -> Result<ListCrawlsResponse, String> {
        if !api_key.has_scope(&ApiKeyScope::CrawlsColonRead) {
            return Ok(ListCrawlsResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope);
        }

        let limit = query_params.limit.unwrap_or(DEFAULT_CRAWLS_LIMIT);
        if !(1..=MAX_CRAWLS_LIMIT).contains(&limit)
            || query_params
                .address
                .as_ref()
                .is_some_and(|address| !(1..=255).contains(&address.chars().count()))
            || query_params.port == Some(0)
            || !is_valid_half_open_range(
                query_params.crawled_from.as_ref(),
                query_params.crawled_to.as_ref(),
            )
        {
            return Ok(ListCrawlsResponse::Status400_TheRequestIsInvalid);
        }
        let limit = limit as usize;

        let cursor = match query_params.cursor.as_deref() {
            Some(cursor) => match CrawlCursor::decode(cursor) {
                Ok(cursor) => Some(cursor),
                Err(_) => {
                    return Ok(ListCrawlsResponse::Status400_TheRequestIsInvalid);
                }
            },
            None => None,
        };

        let mut query = QueryBuilder::<MySql>::new(
            "SELECT id, address, port, ping, version, protocol_version, max_players, \
             online_players, description, favicon, crawled_at FROM crawls WHERE 1 = 1",
        );
        if let Some(address) = &query_params.address {
            query.push(" AND address = ").push_bind(address);
        }
        if let Some(port) = query_params.port {
            query.push(" AND port = ").push_bind(port);
        }
        if let Some(version) = &query_params.version {
            query.push(" AND version = ").push_bind(version);
        }
        if let Some(protocol_version) = query_params.protocol_version {
            query
                .push(" AND protocol_version = ")
                .push_bind(protocol_version);
        }
        if let Some(crawled_from) = query_params.crawled_from {
            query.push(" AND crawled_at >= ").push_bind(crawled_from);
        }
        if let Some(crawled_to) = query_params.crawled_to {
            query.push(" AND crawled_at < ").push_bind(crawled_to);
        }
        if let Some(cursor) = cursor {
            query
                .push(" AND (crawled_at < ")
                .push_bind(cursor.value)
                .push(" OR (crawled_at = ")
                .push_bind(cursor.value)
                .push(" AND id < ")
                .push_bind(cursor.tie_breaker)
                .push("))");
        }
        query
            .push(" ORDER BY crawled_at DESC, id DESC LIMIT ")
            .push_bind((limit + 1) as i64);

        let mut rows = query
            .build_query_as::<CrawlRecord>()
            .fetch_all(&self.default_pool)
            .await
            .map_err(log_database_error)?;

        let next_cursor = if rows.len() > limit {
            rows.pop();
            let last = rows.last().expect("page must include at least one row");
            Some(
                CrawlCursor {
                    value: last.crawled_at,
                    tie_breaker: last.id,
                }
                .encode()
                .map_err(|error| {
                    tracing::error!(?error, "failed to encode crawls cursor");
                    "failed to encode crawls cursor".to_string()
                })?,
            )
        } else {
            None
        };

        let items = rows.into_iter().map(Crawl::from).collect();

        Ok(
            ListCrawlsResponse::Status200_TheCrawlsWereRetrievedSuccessfully(
                ListCrawls200Response::new(
                    items,
                    next_cursor.map_or(Nullable::Null, Nullable::Present),
                ),
            ),
        )
    }
}

fn valid_favicon(favicon: &Nullable<String>) -> bool {
    match favicon {
        Nullable::Present(favicon) => favicon.starts_with("data:image/png;base64,"),
        Nullable::Null => true,
    }
}

fn log_database_error(error: sqlx::Error) -> String {
    tracing::error!(?error, "crawl database operation failed");
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_favicon_data_urls() {
        assert!(valid_favicon(&Nullable::Null));
        assert!(valid_favicon(&Nullable::Present(
            "data:image/png;base64,iVBORw0KGgo=".to_string()
        )));
        assert!(!valid_favicon(&Nullable::Present(
            "https://example.com/favicon.png".to_string()
        )));
    }
}

use graph_api::models::ApiKey;
use std::future::Future;

tokio::task_local! {
    static API_KEY: ApiKey;
}

pub(crate) async fn with_api_key<F>(api_key: ApiKey, future: F) -> F::Output
where
    F: Future,
{
    API_KEY.scope(api_key, future).await
}

pub(crate) fn current_api_key() -> Result<ApiKey, String> {
    API_KEY
        .try_with(Clone::clone)
        .map_err(|_| "authenticated API key is missing from the request context".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use graph_api::types::Nullable;

    #[tokio::test]
    async fn api_key_is_available_only_inside_its_request_context() {
        let api_key = ApiKey::new(
            "Test API key".to_string(),
            "test-public-id".to_string(),
            vec!["*".to_string()],
            Utc::now(),
            Nullable::Null,
        );

        with_api_key(api_key.clone(), async {
            assert_eq!(current_api_key().unwrap(), api_key);
        })
        .await;
        assert!(current_api_key().is_err());
    }
}

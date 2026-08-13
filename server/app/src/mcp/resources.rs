mod players;

use crate::mcp::Mcp;
use graph_api::models::ApiKey;
use rmcp::ErrorData;
use rmcp::model::{ReadResourceResponse, ResourceTemplate};

pub(super) fn templates() -> Vec<ResourceTemplate> {
    vec![players::template()]
}

impl Mcp {
    pub(super) async fn route_resource(
        &self,
        api_key: &ApiKey,
        uri: &str,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if let Some(player_id) = players::parse_uri(uri) {
            return self.read_player_resource(api_key, player_id, uri).await;
        }

        Err(ErrorData::resource_not_found("Resource not found", None))
    }
}

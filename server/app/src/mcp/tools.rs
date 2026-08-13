mod network;
mod patch_notes;
mod players;
mod punishments;

use crate::mcp::Mcp;
use graph_api::models::ApiKey;
use rmcp::ErrorData;
use rmcp::model::{CallToolRequestParams, CallToolResponse, Tool};

pub(super) fn list() -> Vec<Tool> {
    let mut tools = Vec::new();
    tools.extend(players::tools());
    tools.extend(network::tools());
    tools.extend(punishments::tools());
    tools.extend(patch_notes::tools());
    tools
}

impl Mcp {
    pub(super) async fn route_tool(
        &self,
        api_key: &ApiKey,
        request: CallToolRequestParams,
    ) -> Result<CallToolResponse, ErrorData> {
        match request.name.as_ref() {
            "get_player_overview" => {
                let args = serde_json::from_value(
                    request
                        .arguments
                        .map(serde_json::Value::Object)
                        .unwrap_or(serde_json::Value::Null),
                )
                .map_err(|e| {
                    ErrorData::invalid_params(format!("Invalid arguments: {}", e), None)
                })?;
                Ok(CallToolResponse::Complete(
                    self.get_player_overview(api_key, args).await?,
                ))
            }
            "get_player_relationships" => {
                let args = serde_json::from_value(
                    request
                        .arguments
                        .map(serde_json::Value::Object)
                        .unwrap_or(serde_json::Value::Null),
                )
                .map_err(|e| {
                    ErrorData::invalid_params(format!("Invalid arguments: {}", e), None)
                })?;
                Ok(CallToolResponse::Complete(
                    self.get_player_relationships(api_key, args).await?,
                ))
            }
            "get_population_trend" => {
                let args = serde_json::from_value(
                    request
                        .arguments
                        .map(serde_json::Value::Object)
                        .unwrap_or(serde_json::Value::Null),
                )
                .map_err(|e| {
                    ErrorData::invalid_params(format!("Invalid arguments: {}", e), None)
                })?;
                Ok(CallToolResponse::Complete(
                    self.get_population_trend(api_key, args).await?,
                ))
            }
            "get_network_status" => Ok(CallToolResponse::Complete(
                self.get_network_status(api_key).await?,
            )),
            "search_punishments" => {
                let args = serde_json::from_value(
                    request
                        .arguments
                        .map(serde_json::Value::Object)
                        .unwrap_or(serde_json::Value::Null),
                )
                .map_err(|e| {
                    ErrorData::invalid_params(format!("Invalid arguments: {}", e), None)
                })?;
                Ok(CallToolResponse::Complete(
                    self.search_punishments(api_key, args).await?,
                ))
            }
            "search_patch_notes" => {
                let args = serde_json::from_value(
                    request
                        .arguments
                        .map(serde_json::Value::Object)
                        .unwrap_or(serde_json::Value::Null),
                )
                .map_err(|e| {
                    ErrorData::invalid_params(format!("Invalid arguments: {}", e), None)
                })?;
                Ok(CallToolResponse::Complete(
                    self.search_patch_notes(api_key, args).await?,
                ))
            }
            _ => Err(ErrorData::invalid_request(
                format!("Tool not found: {}", request.name),
                None,
            )),
        }
    }
}

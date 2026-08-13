use crate::mcp::Mcp;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ContentBlock, GetPromptResult, PromptMessage, Role, TextContent};
use rmcp::{ErrorData, prompt, prompt_router};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
pub(super) struct AnalyzePlayerArgs {
    /// Minecraft username or UUID
    pub player: String,
}

#[prompt_router(vis = "pub(super)")]
impl Mcp {
    #[prompt(
        name = "analyze_player",
        description = "Comprehensive investigation workflow for a specific player."
    )]
    pub(super) async fn analyze_player(
        &self,
        params: Parameters<AnalyzePlayerArgs>,
    ) -> Result<GetPromptResult, ErrorData> {
        let player = params.0.player;

        Ok(GetPromptResult::new(vec![PromptMessage::new(
            Role::User,
            ContentBlock::Text(TextContent::new(format!(
                "Please analyze the player '{}'. Use get_player_overview first, then search_punishments and get_player_relationships to assess their history and activity.",
                player
            ))),
        )])
        .with_description("Analyze Player"))
    }

    #[prompt(
        name = "network_health_report",
        description = "Workflow to analyze server population health and recent updates."
    )]
    pub(super) async fn network_health_report(&self) -> Result<GetPromptResult, ErrorData> {
        Ok(GetPromptResult::new(vec![PromptMessage::new(
            Role::User,
            ContentBlock::Text(TextContent::new(
                "Please perform a network health report. Check get_network_status for all servers, and if you find any anomalies in population, use get_population_trend and search_patch_notes to investigate potential causes."
            )),
        )])
        .with_description("Network Health Report"))
    }
}

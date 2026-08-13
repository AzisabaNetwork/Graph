use rmcp::ErrorData;
use rmcp::model::{
    ContentBlock, GetPromptRequestParams, GetPromptResponse, GetPromptResult, Prompt,
    PromptArgument, PromptMessage, Role, TextContent,
};

pub(super) fn list() -> Vec<Prompt> {
    vec![
        Prompt::new(
            "analyze_player",
            Some("Comprehensive investigation workflow for a specific player.".to_string()),
            Some(vec![
                PromptArgument::new("player")
                    .with_description("Minecraft username or UUID")
                    .with_required(true),
            ]),
        ),
        Prompt::new(
            "network_health_report",
            Some("Workflow to analyze server population health and recent updates.".to_string()),
            None,
        ),
    ]
}

pub(super) fn get(request: GetPromptRequestParams) -> Result<GetPromptResponse, ErrorData> {
    match request.name.as_str() {
        "analyze_player" => {
            let player = request
                .arguments
                .as_ref()
                .and_then(|args| args.get("player"))
                .ok_or_else(|| ErrorData::invalid_params("missing required argument: player", None))?;

            Ok(GetPromptResponse::Complete(
                GetPromptResult::new(vec![PromptMessage::new(
                    Role::User,
                    ContentBlock::Text(TextContent::new(format!(
                        "Please analyze the player '{}'. Use get_player_overview first, then search_punishments and get_player_relationships to assess their history and activity.",
                        player
                    ))),
                )])
                .with_description("Analyze Player"),
            ))
        }
        "network_health_report" => Ok(GetPromptResponse::Complete(
            GetPromptResult::new(vec![PromptMessage::new(
                Role::User,
                ContentBlock::Text(TextContent::new(
                    "Please perform a network health report. Check get_network_status for all servers, and if you find any anomalies in population, use get_population_trend and search_patch_notes to investigate potential causes."
                )),
            )])
            .with_description("Network Health Report"),
        )),
        _ => Err(ErrorData::invalid_request("Prompt not found", None)),
    }
}

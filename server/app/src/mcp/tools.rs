mod network;
mod patch_notes;
mod players;
mod punishments;

use crate::mcp::Mcp;
use rmcp::handler::server::tool::ToolRouter;

pub(super) fn tool_router() -> ToolRouter<Mcp> {
    let mut router = Mcp::players_tools();
    router.merge(Mcp::network_tools());
    router.merge(Mcp::punishments_tools());
    router.merge(Mcp::patch_notes_tools());
    router
}

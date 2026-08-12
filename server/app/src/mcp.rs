use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::ServerHandler;

#[derive(Clone, Debug)]
pub(crate) struct Mcp;

impl ServerHandler for Mcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::default())
    }
}

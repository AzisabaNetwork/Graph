use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct ResourceLink {
    pub uri: String,
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct PlayerOverview {
    pub id: Uuid,
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discord_id: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
    pub first_login_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_server: Option<String>,
    pub punishment_count: u64,
    pub active_punishment_count: u64,
    pub friend_count: u64,
    pub resource_link: ResourceLink,
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct PlayerRelationships {
    pub friend_count: u64,
    pub friends: Vec<FriendSummary>,
    pub incoming_requests_count: u64,
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct FriendSummary {
    pub id: Uuid,
    pub username: String,
    pub status: String,
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct PunishmentSummary {
    pub id: u64,
    pub r#type: String,
    pub reason: String,
    pub server: String,
    pub created_at: DateTime<Utc>,
    pub active: bool,
    pub resource_link: ResourceLink,
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct PopulationTrend {
    pub address: String,
    pub port: u16,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub interval: String,
    pub points: Vec<PopulationPoint>,
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct PopulationPoint {
    pub timestamp: DateTime<Utc>,
    pub avg_online: f64,
    pub max_online: u32,
    pub sample_count: u64,
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct NetworkOverview {
    pub servers: Vec<ServerStatus>,
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct ServerStatus {
    pub address: String,
    pub port: u16,
    pub online_players: u32,
    pub max_players: u32,
    pub version: String,
    pub crawled_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct PatchNoteSummary {
    pub id: Uuid,
    pub title: String,
    pub category: String,
    pub created_at: DateTime<Utc>,
    pub resource_link: ResourceLink,
}

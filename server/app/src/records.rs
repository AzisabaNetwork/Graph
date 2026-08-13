use chrono::{DateTime, Utc};
use graph_api::models::{
    ApiKey, Crawl, PatchNote, Player, PlayerStatus, Proof, Punishment, PunishmentType, Revocation1,
};
use graph_api::types::Nullable;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, FromRow)]
pub(crate) struct ApiKeyRecord {
    pub(crate) name: String,
    pub(crate) public_id: String,
    pub(crate) secret_digest: Vec<u8>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) expires_at: Option<DateTime<Utc>>,
    pub(crate) player_id: Option<Uuid>,
}

impl ApiKeyRecord {
    pub(crate) fn into_api_key(self, scopes: Vec<String>) -> ApiKey {
        ApiKey::new(
            self.name,
            self.public_id,
            self.player_id.map_or(Nullable::Null, Nullable::Present),
            scopes,
            self.created_at,
            self.expires_at.map_or(Nullable::Null, Nullable::Present),
        )
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct CrawlRecord {
    pub(crate) id: Uuid,
    pub(crate) address: String,
    pub(crate) port: u16,
    pub(crate) ping: u32,
    pub(crate) version: String,
    pub(crate) protocol_version: i32,
    pub(crate) max_players: u32,
    pub(crate) online_players: u32,
    pub(crate) description: Option<String>,
    pub(crate) favicon: Option<String>,
    pub(crate) crawled_at: DateTime<Utc>,
}

impl From<CrawlRecord> for Crawl {
    fn from(record: CrawlRecord) -> Self {
        Self {
            id: record.id,
            address: record.address,
            port: record.port,
            ping: record.ping,
            version: record.version,
            protocol_version: record.protocol_version,
            max_players: record.max_players,
            online_players: record.online_players,
            description: record.description,
            favicon: record.favicon.map_or(Nullable::Null, Nullable::Present),
            crawled_at: record.crawled_at,
        }
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct PatchNoteRecord {
    pub(crate) id: Uuid,
    pub(crate) target: String,
    pub(crate) category: String,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) author_id: Option<Uuid>,
    pub(crate) created_at: DateTime<Utc>,
}

impl PatchNoteRecord {
    pub(crate) fn into_patch_note(self, image_urls: Vec<String>) -> PatchNote {
        PatchNote::new(
            self.id,
            self.target,
            self.category,
            self.title,
            self.body,
            self.author_id.map_or(Nullable::Null, Nullable::Present),
            image_urls,
            self.created_at,
        )
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct PlayerRecord {
    pub(crate) id: Uuid,
    pub(crate) discord_id: Option<String>,
    pub(crate) bio: Option<String>,
    pub(crate) status: String,
    pub(crate) current_server: Option<String>,
    pub(crate) current_locale: Option<String>,
    pub(crate) current_client_version: Option<String>,
    pub(crate) first_login_at: Option<DateTime<Utc>>,
    pub(crate) last_seen_at: Option<DateTime<Utc>>,
}

impl PlayerRecord {
    pub(crate) fn empty(id: Uuid) -> Self {
        Self {
            id,
            discord_id: None,
            bio: None,
            status: PlayerStatus::Offline.to_string(),
            current_server: None,
            current_locale: None,
            current_client_version: None,
            first_login_at: None,
            last_seen_at: None,
        }
    }

    pub(crate) fn into_player(self, username: String, with_details: bool) -> Player {
        Player::new(
            self.id,
            with_details
                .then_some(self.discord_id)
                .flatten()
                .map_or(Nullable::Null, Nullable::Present),
            username,
            self.bio.map_or(Nullable::Null, Nullable::Present),
            self.status,
            self.current_server
                .map_or(Nullable::Null, Nullable::Present),
            self.current_locale
                .map_or(Nullable::Null, Nullable::Present),
            self.current_client_version
                .map_or(Nullable::Null, Nullable::Present),
            self.first_login_at
                .map_or(Nullable::Null, Nullable::Present),
            self.last_seen_at.map_or(Nullable::Null, Nullable::Present),
        )
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct PunishmentRecord {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) target: String,
    pub(crate) reason: String,
    pub(crate) operator: String,
    pub(crate) r#type: String,
    pub(crate) start: i64,
    pub(crate) end: i64,
    pub(crate) server: String,
    pub(crate) extra: String,
    pub(crate) active: bool,
    pub(crate) revocation_id: Option<i64>,
    pub(crate) revocation_reason: Option<String>,
    pub(crate) revocation_timestamp: Option<i64>,
    pub(crate) revocation_operator: Option<String>,
}

impl PunishmentRecord {
    pub(crate) fn into_punishment(self, proofs: Vec<Proof>) -> Result<Punishment, String> {
        let punishment_type = punishment_type_from_database(&self.r#type)
            .ok_or_else(|| format!("unknown punishment type in database: {}", self.r#type))?;
        let actor_id = Uuid::parse_str(&self.operator)
            .map_err(|_| "invalid punishment operator UUID in database".to_string())?;
        let expires_at = if self.end == -1 {
            Nullable::Null
        } else {
            Nullable::Present(datetime_from_millis(self.end)?)
        };
        let revocation = match (
            self.revocation_id,
            self.revocation_reason,
            self.revocation_timestamp,
            self.revocation_operator,
        ) {
            (Some(id), Some(reason), Some(timestamp), Some(operator)) => {
                Nullable::Present(Revocation1::new(
                    unsigned_row_id(id)?,
                    reason,
                    Uuid::parse_str(&operator)
                        .map_err(|_| "invalid revocation operator UUID in database".to_string())?,
                    datetime_from_millis(timestamp)?,
                ))
            }
            _ => Nullable::Null,
        };

        Ok(Punishment::new(
            unsigned_row_id(self.id)?,
            self.name,
            self.target,
            punishment_type.to_string(),
            self.reason,
            actor_id,
            datetime_from_millis(self.start)?,
            expires_at,
            self.server,
            self.extra.split(',').any(|flag| flag == "SEEN"),
            self.active,
            proofs,
            revocation,
        ))
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct ProofRecord {
    pub(crate) id: i64,
    pub(crate) text: String,
    pub(crate) public: bool,
}

impl TryFrom<ProofRecord> for Proof {
    type Error = String;

    fn try_from(record: ProofRecord) -> Result<Self, Self::Error> {
        Ok(Self::new(
            unsigned_row_id(record.id)?,
            record.text,
            record.public,
        ))
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct PunishmentProofRecord {
    pub(crate) punish_id: i64,
    pub(crate) id: i64,
    pub(crate) text: String,
    pub(crate) public: bool,
}

impl PunishmentProofRecord {
    pub(crate) fn into_proof(self) -> Result<Proof, String> {
        ProofRecord {
            id: self.id,
            text: self.text,
            public: self.public,
        }
        .try_into()
    }
}

fn punishment_type_from_database(value: &str) -> Option<PunishmentType> {
    Some(match value {
        "BAN" => PunishmentType::Ban,
        "TEMP_BAN" => PunishmentType::TempBan,
        "IP_BAN" => PunishmentType::IpBan,
        "TEMP_IP_BAN" => PunishmentType::TempIpBan,
        "MUTE" => PunishmentType::Mute,
        "TEMP_MUTE" => PunishmentType::TempMute,
        "IP_MUTE" => PunishmentType::IpMute,
        "TEMP_IP_MUTE" => PunishmentType::TempIpMute,
        "WARNING" => PunishmentType::Warning,
        "CAUTION" => PunishmentType::Caution,
        "KICK" => PunishmentType::Kick,
        "NOTE" => PunishmentType::Note,
        _ => return None,
    })
}

fn datetime_from_millis(value: i64) -> Result<DateTime<Utc>, String> {
    DateTime::from_timestamp_millis(value)
        .ok_or_else(|| "invalid epoch milliseconds in punishments database".to_string())
}

fn unsigned_row_id(value: i64) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| "negative identifier in punishments database".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_punishment_types_map_to_generated_models() {
        for (database, api) in [
            ("BAN", "ban"),
            ("TEMP_BAN", "tempBan"),
            ("IP_BAN", "ipBan"),
            ("TEMP_IP_BAN", "tempIpBan"),
            ("MUTE", "mute"),
            ("TEMP_MUTE", "tempMute"),
            ("IP_MUTE", "ipMute"),
            ("TEMP_IP_MUTE", "tempIpMute"),
            ("WARNING", "warning"),
            ("CAUTION", "caution"),
            ("KICK", "kick"),
            ("NOTE", "note"),
        ] {
            assert_eq!(
                punishment_type_from_database(database),
                Some(api.parse::<PunishmentType>().unwrap())
            );
        }
    }

    #[test]
    fn proof_conversion_rejects_negative_database_ids() {
        let record = ProofRecord {
            id: -1,
            text: "proof".to_string(),
            public: true,
        };

        assert!(Proof::try_from(record).is_err());
    }
}

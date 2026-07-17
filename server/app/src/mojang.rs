use reqwest::Client;
use serde::Deserialize;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::time::Duration;
use uuid::Uuid;

const API_BASE_URL: &str = "https://playerdb.co/api/player/minecraft";
const USER_AGENT: &str = "Azisaba-Graph/1.0 (https://github.com/AzisabaNetwork/Graph)";

#[derive(Debug, Clone)]
pub(crate) struct PlayerDbClient {
    client: Client,
}

impl PlayerDbClient {
    pub fn new() -> Result<Self, PlayerDbError> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(10))
            .build()?;

        Ok(Self { client })
    }

    async fn find_player(
        &self,
        identifier: impl Display,
    ) -> Result<Option<PlayerDbPlayer>, PlayerDbError> {
        let response = self
            .client
            .get(format!("{API_BASE_URL}/{identifier}"))
            .send()
            .await?
            .json::<PlayerDbResponse>()
            .await?;

        match response {
            PlayerDbResponse::Found { data } => Ok(Some(data.player)),
            PlayerDbResponse::NotFound => Ok(None),
            PlayerDbResponse::Unknown => Err(PlayerDbError::UnexpectedResponse),
        }
    }

    pub async fn get_username_by_uuid(&self, id: Uuid) -> Result<Option<String>, PlayerDbError> {
        let Some(player) = self.find_player(id).await? else {
            return Ok(None);
        };
        if player.id != id {
            return Err(PlayerDbError::UnexpectedResponse);
        }
        Ok(Some(player.username))
    }
}

#[derive(Debug)]
pub(crate) enum PlayerDbError {
    Request(reqwest::Error),
    UnexpectedResponse,
}

impl Display for PlayerDbError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            PlayerDbError::Request(error) => write!(f, "PlayerDB request failed: {error}"),
            PlayerDbError::UnexpectedResponse => {
                write!(f, "PlayerDB returned an unexpected response")
            }
        }
    }
}

impl Error for PlayerDbError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            PlayerDbError::Request(error) => Some(error),
            PlayerDbError::UnexpectedResponse => None,
        }
    }
}

impl From<reqwest::Error> for PlayerDbError {
    fn from(error: reqwest::Error) -> Self {
        PlayerDbError::Request(error)
    }
}

impl From<PlayerDbError> for String {
    fn from(error: PlayerDbError) -> Self {
        error.to_string()
    }
}

#[derive(Debug, Deserialize)]
struct PlayerDbPlayer {
    id: Uuid,
    username: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "code")]
enum PlayerDbResponse {
    #[serde(rename = "player.found")]
    Found { data: PlayerDbFoundData },

    #[serde(rename = "minecraft.invalid_username")]
    NotFound,

    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct PlayerDbFoundData {
    player: PlayerDbPlayer,
}

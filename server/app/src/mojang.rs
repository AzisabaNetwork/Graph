use reqwest::Client;
use serde::Deserialize;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::time::Duration;
use uuid::Uuid;

const API_BASE_URL: &str = "https://playerdb.co/api/player/minecraft";
const USER_AGENT: &str = "Azisaba-Graph/1.0 (https://github.com/AzisabaNetwork/Graph)";

#[derive(Debug, Clone)]
pub(crate) struct MojangProfileResolver {
    client: Client,
}

#[derive(Debug)]
pub(crate) enum MojangProfileResolverError {
    Request(reqwest::Error),
    UnexpectedResponse,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MojangProfile {
    pub(crate) id: Uuid,
    pub(crate) username: String,
}

impl MojangProfileResolver {
    pub fn new() -> Result<Self, MojangProfileResolverError> {
        Ok(Self {
            client: Client::builder()
                .user_agent(USER_AGENT)
                .timeout(Duration::from_secs(10))
                .build()?,
        })
    }

    pub async fn find_by_uuid(
        &self,
        id: Uuid,
    ) -> Result<Option<MojangProfile>, MojangProfileResolverError> {
        let Some(profile) = self.request_profile(id).await? else {
            return Ok(None);
        };

        if profile.id != id {
            return Err(MojangProfileResolverError::UnexpectedResponse);
        }

        Ok(Some(profile))
    }

    pub async fn find_by_username(
        &self,
        username: &str,
    ) -> Result<Option<MojangProfile>, MojangProfileResolverError> {
        let Some(profile) = self.request_profile(username).await? else {
            return Ok(None);
        };

        if !profile.username.eq_ignore_ascii_case(username) {
            return Err(MojangProfileResolverError::UnexpectedResponse);
        }

        Ok(Some(profile))
    }

    async fn request_profile(
        &self,
        identifier: impl Display,
    ) -> Result<Option<MojangProfile>, MojangProfileResolverError> {
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
            PlayerDbResponse::Unknown => Err(MojangProfileResolverError::UnexpectedResponse),
        }
    }
}

impl Display for MojangProfileResolverError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(error) => write!(f, "Mojang profile request failed: {error}"),
            Self::UnexpectedResponse => {
                f.write_str("Mojang profile resolver returned an unexpected response")
            }
        }
    }
}

impl Error for MojangProfileResolverError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Request(error) => Some(error),
            Self::UnexpectedResponse => None,
        }
    }
}

impl From<reqwest::Error> for MojangProfileResolverError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<MojangProfileResolverError> for String {
    fn from(error: MojangProfileResolverError) -> Self {
        error.to_string()
    }
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
    player: MojangProfile,
}

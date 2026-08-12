use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Cursor<V, T> {
    pub(crate) value: V,
    pub(crate) tie_breaker: T,
}

#[derive(Debug)]
pub(crate) enum CursorError {
    InvalidJson,
    InvalidBase64,
}

impl<V, T> Cursor<V, T>
where
    V: Serialize,
    T: Serialize,
{
    pub(crate) fn encode(&self) -> Result<String, CursorError> {
        let json = serde_json::to_vec(self).map_err(|_| CursorError::InvalidJson)?;

        Ok(URL_SAFE_NO_PAD.encode(json))
    }
}

impl<V, T> Cursor<V, T>
where
    V: for<'de> Deserialize<'de>,
    T: for<'de> Deserialize<'de>,
{
    pub(crate) fn decode(value: &str) -> Result<Self, CursorError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| CursorError::InvalidBase64)?;

        serde_json::from_slice(&bytes).map_err(|_| CursorError::InvalidJson)
    }
}

/*
 * Azisaba Graph API
 *
 * This file is maintained separately from the generated API implementation
 * because OpenAPI Generator does not expose text/event-stream as a typed Rust
 * stream.
 */

use futures_core::Stream;
use futures_util::StreamExt;
use reqwest;
use serde::{Deserialize, Serialize};

use super::{configuration, Error};
use crate::{apis::ResponseContent, models};

/// Typed errors returned while opening the event stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StreamEventsError {
    Status401(),
    Status403(),
    UnknownValue(serde_json::Value),
}

/// Opens the Server-Sent Events connection and yields deserialized Graph events.
///
/// Dropping the returned stream closes the underlying HTTP response. The stream
/// ends when the server disconnects and does not reconnect automatically.
pub async fn stream_events(
    configuration: &configuration::Configuration,
) -> Result<
    impl Stream<Item = Result<models::StreamEvent, Error<StreamEventsError>>>,
    Error<StreamEventsError>,
> {
    let uri = format!("{}/stream", configuration.base_path);
    let mut request = configuration
        .client
        .request(reqwest::Method::GET, uri)
        .header(reqwest::header::ACCEPT, "text/event-stream");

    if let Some(ref user_agent) = configuration.user_agent {
        request = request.header(reqwest::header::USER_AGENT, user_agent.clone());
    }
    if let Some(ref token) = configuration.bearer_access_token {
        request = request.bearer_auth(token);
    }

    let response = configuration.client.execute(request.build()?).await?;
    let status = response.status();

    if status.is_client_error() || status.is_server_error() {
        let content = response.text().await?;
        let entity = serde_json::from_str(&content).ok();
        return Err(Error::ResponseError(ResponseContent {
            status,
            content,
            entity,
        }));
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type
        .to_ascii_lowercase()
        .starts_with("text/event-stream")
    {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unexpected stream Content-Type: {content_type}"),
        )));
    }

    let mut chunks = response.bytes_stream();
    Ok(async_stream::try_stream! {
        let mut parser = EventStreamParser::default();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk?;
            for payload in parser.push(&chunk, false)? {
                yield serde_json::from_str::<models::StreamEvent>(&payload)?;
            }
        }
        for payload in parser.push(&[], true)? {
            yield serde_json::from_str::<models::StreamEvent>(&payload)?;
        }
    })
}

#[derive(Default)]
struct EventStreamParser {
    buffer: Vec<u8>,
    data: Vec<String>,
}

impl EventStreamParser {
    fn push(&mut self, chunk: &[u8], end_of_stream: bool) -> std::io::Result<Vec<String>> {
        self.buffer.extend_from_slice(chunk);
        let mut payloads = Vec::new();

        while let Some((line, consumed)) = next_line(&self.buffer, end_of_stream) {
            let line = String::from_utf8(line.to_vec())
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            self.buffer.drain(..consumed);
            if let Some(payload) = self.consume_line(&line) {
                payloads.push(payload);
            }
        }

        if end_of_stream {
            if let Some(payload) = self.dispatch() {
                payloads.push(payload);
            }
        }
        Ok(payloads)
    }

    fn consume_line(&mut self, line: &str) -> Option<String> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }

        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        if field == "data" {
            self.data.push(value.to_owned());
        }
        None
    }

    fn dispatch(&mut self) -> Option<String> {
        if self.data.is_empty() {
            return None;
        }
        Some(std::mem::take(&mut self.data).join("\n"))
    }
}

fn next_line(buffer: &[u8], end_of_stream: bool) -> Option<(&[u8], usize)> {
    for (index, byte) in buffer.iter().enumerate() {
        match byte {
            b'\n' => return Some((&buffer[..index], index + 1)),
            b'\r' => {
                if index + 1 == buffer.len() && !end_of_stream {
                    return None;
                }
                let consumed = if buffer.get(index + 1) == Some(&b'\n') {
                    index + 2
                } else {
                    index + 1
                };
                return Some((&buffer[..index], consumed));
            }
            _ => {}
        }
    }

    if end_of_stream && !buffer.is_empty() {
        Some((buffer, buffer.len()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::EventStreamParser;
    use crate::models::StreamEvent;

    const PLAYER: &str = r#"{
        "id":"00000000-0000-0000-0000-000000000001",
        "discordId":null,
        "username":"player",
        "status":"offline",
        "currentServer":null,
        "bio":null
    }"#;

    #[test]
    fn parses_chunked_multiline_events_and_ignores_comments() {
        let mut parser = EventStreamParser::default();
        assert!(parser
            .push(b": keep-alive\r\nda", false)
            .unwrap()
            .is_empty());
        assert_eq!(
            parser
                .push(b"ta: {\"type\":\r\ndata: \"friend-added\"}\r\n\r\n", false)
                .unwrap(),
            vec!["{\"type\":\n\"friend-added\"}"]
        );
    }

    #[test]
    fn dispatches_the_last_event_when_the_connection_closes() {
        let mut parser = EventStreamParser::default();
        assert_eq!(
            parser
                .push(b"data: {\"type\":\"friend-added\"}", true)
                .unwrap(),
            vec!["{\"type\":\"friend-added\"}"]
        );
    }

    #[test]
    fn dispatches_structurally_identical_events_by_type() {
        let payload = format!(
            r#"{{"type":"friend-removed","data":{{"player":{PLAYER},"friend":{PLAYER}}}}}"#
        );

        assert!(matches!(
            serde_json::from_str::<StreamEvent>(&payload).unwrap(),
            StreamEvent::FriendRemoved(_)
        ));
    }

    #[test]
    fn rejects_unknown_event_types() {
        let error =
            serde_json::from_str::<StreamEvent>(r#"{"type":"unknown","data":{}}"#).unwrap_err();
        assert!(error.to_string().contains("unknown variant"));
    }
}

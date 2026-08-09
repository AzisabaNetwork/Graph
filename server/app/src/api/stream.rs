use crate::api::Api;
use crate::auth::scope::ApiKeyScopeExt;
use async_trait::async_trait;
use axum::response::sse::Event;
use axum_extra::extract::CookieJar;
use graph_api::apis::stream::{Stream, StreamEventsResponse};
use graph_api::models::{ApiKey, ApiKeyScope, FriendRequestAcceptedEvent, FriendRequestAcceptedEventData, FriendRequestAddedEvent, FriendRequestAddedEventData, FriendRequestRejectedEvent, FriendRequestRejectedEventData, FriendRequestRemovedEvent, FriendRequestRemovedEventData, Player, SSE, StreamEvent};
use graph_api::types::Object;
use headers::Host;
use http::Method;
use redis::AsyncCommands;
use std::convert::Infallible;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::{BroadcastStream, IntervalStream};

const STREAM_EVENTS_CHANNEL: &str = "graph:stream-events";
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);
const REDIS_RECONNECT_DELAY: Duration = Duration::from_secs(1);

pub(crate) fn friend_request_accepted_event(sender: Player, receiver: Player) -> StreamEvent {
    FriendRequestAcceptedEvent::new(
        event_type("friend-request-accepted"),
        FriendRequestAcceptedEventData::new(sender, receiver),
    )
    .into()
}

pub(crate) fn friend_request_added_event(sender: Player, receiver: Player) -> StreamEvent {
    FriendRequestAddedEvent::new(
        event_type("friend-request-added"),
        FriendRequestAddedEventData::new(sender, receiver),
    )
    .into()
}

pub(crate) fn friend_request_rejected_event(sender: Player, receiver: Player) -> StreamEvent {
    FriendRequestRejectedEvent::new(
        event_type("friend-request-rejected"),
        FriendRequestRejectedEventData::new(sender, receiver),
    )
    .into()
}

pub(crate) fn friend_request_removed_event(sender: Player, receiver: Player) -> StreamEvent {
    FriendRequestRemovedEvent::new(
        event_type("friend-request-removed"),
        FriendRequestRemovedEventData::new(sender, receiver),
    )
    .into()
}

fn event_type(value: &'static str) -> Object {
    Object(serde_json::Value::String(value.to_string()))
}

fn stream_event_type(event: &StreamEvent) -> Option<&str> {
    let value = match event {
        StreamEvent::FriendRequestAcceptedEvent(event) => &event.r_type.0,
        StreamEvent::FriendRequestAddedEvent(event) => &event.r_type.0,
        StreamEvent::FriendRequestRejectedEvent(event) => &event.r_type.0,
        StreamEvent::FriendRequestRemovedEvent(event) => &event.r_type.0,
    };
    value.as_str()
}

fn is_visible_to(event: &StreamEvent, api_key: &ApiKey) -> bool {
    match event {
        StreamEvent::FriendRequestAcceptedEvent(_)
        | StreamEvent::FriendRequestAddedEvent(_)
        | StreamEvent::FriendRequestRejectedEvent(_)
        | StreamEvent::FriendRequestRemovedEvent(_) => {
            api_key.has_scope(&ApiKeyScope::PlayersColonRead)
                || api_key.has_scope(&ApiKeyScope::PlayersColonReadDetails)
        }
    }
}

fn into_sse(event: StreamEvent) -> Result<Event, axum::Error> {
    let event_type = stream_event_type(&event).unwrap_or("stream-event");
    Event::default().event(event_type).json_data(event)
}

impl Api {
    pub(crate) async fn publish_stream_event(&self, event: StreamEvent) {
        let Some(redis) = &self.redis else {
            let _ = self.stream_events.send(event);
            return;
        };
        let payload = match serde_json::to_string(&event) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(%error, "failed to serialize stream event");
                return;
            }
        };
        let mut redis = redis.clone();
        match redis
            .publish::<_, _, usize>(STREAM_EVENTS_CHANNEL, payload)
            .await
        {
            Ok(0) => tracing::warn!(
                channel = STREAM_EVENTS_CHANNEL,
                "stream event had no Redis subscribers"
            ),
            Ok(_) => {}
            Err(error) => {
                tracing::error!(%error, "failed to publish stream event to Redis");
            }
        }
    }

    pub(crate) async fn start_stream_event_listener(
        &self,
        redis_client: redis::Client,
    ) -> redis::RedisResult<()> {
        let initial_subscription = subscribe_to_stream_events(&redis_client).await?;
        let stream_events = self.stream_events.clone();
        tokio::spawn(async move {
            let mut subscription = Some(initial_subscription);
            loop {
                let pubsub = match subscription.take() {
                    Some(pubsub) => pubsub,
                    None => match subscribe_to_stream_events(&redis_client).await {
                        Ok(pubsub) => pubsub,
                        Err(error) => {
                            tracing::error!(%error, "failed to resubscribe to Redis stream events");
                            tokio::time::sleep(REDIS_RECONNECT_DELAY).await;
                            continue;
                        }
                    },
                };
                if let Err(error) = receive_stream_events(pubsub, &stream_events).await {
                    tracing::error!(%error, "Redis stream event listener disconnected");
                }
                tokio::time::sleep(REDIS_RECONNECT_DELAY).await;
            }
        });
        Ok(())
    }
}

async fn subscribe_to_stream_events(
    redis_client: &redis::Client,
) -> redis::RedisResult<redis::aio::PubSub> {
    let mut pubsub = redis_client.get_async_pubsub().await?;
    pubsub.subscribe(STREAM_EVENTS_CHANNEL).await?;
    tracing::info!(
        channel = STREAM_EVENTS_CHANNEL,
        "subscribed to Redis stream events"
    );
    Ok(pubsub)
}

async fn receive_stream_events(
    pubsub: redis::aio::PubSub,
    stream_events: &broadcast::Sender<StreamEvent>,
) -> redis::RedisResult<()> {
    let mut messages = pubsub.into_on_message();
    while let Some(message) = messages.next().await {
        let payload = message.get_payload::<String>()?;
        match serde_json::from_str::<StreamEvent>(&payload) {
            Ok(event) => {
                let _ = stream_events.send(event);
            }
            Err(error) => {
                tracing::error!(%error, "failed to deserialize Redis stream event");
            }
        }
    }
    Ok(())
}

#[async_trait]
impl Stream<String> for Api {
    type Claims = ApiKey;

    async fn stream_events(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        api_key: &Self::Claims,
    ) -> Result<StreamEventsResponse, String> {
        if !api_key.has_scope(&ApiKeyScope::StreamColonRead) {
            return Ok(StreamEventsResponse::Status403_TheAuthenticatedAPIKeyLacksTheRequiredScope);
        }

        let api_key = api_key.clone();
        let events =
            BroadcastStream::new(self.stream_events.subscribe()).filter_map(move |result| {
                let event = match result {
                    Ok(event) => event,
                    Err(error) => {
                        tracing::warn!(%error, "SSE client lagged behind the event stream");
                        return None;
                    }
                };
                if !is_visible_to(&event, &api_key) {
                    return None;
                }
                match into_sse(event) {
                    Ok(event) => Some(Ok::<_, Infallible>(event)),
                    Err(error) => {
                        tracing::error!(%error, "failed to serialize SSE event");
                        None
                    }
                }
            });
        let keep_alive = IntervalStream::new(tokio::time::interval(KEEP_ALIVE_INTERVAL))
            .map(|_| Ok::<_, Infallible>(Event::default().comment("keep-alive")));
        let stream: SSE = Box::pin(events.merge(keep_alive));

        Ok(StreamEventsResponse::Status200_TheEventStreamWasEstablishedSuccessfully(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use graph_api::types::Nullable;

    #[test]
    fn friend_request_events_require_player_read_access() {
        let event = event();

        assert!(!is_visible_to(
            &event,
            &api_key(&[ApiKeyScope::StreamColonRead])
        ));
        assert!(is_visible_to(
            &event,
            &api_key(&[ApiKeyScope::StreamColonRead, ApiKeyScope::PlayersColonRead,])
        ));
        assert!(is_visible_to(
            &event,
            &api_key(&[
                ApiKeyScope::StreamColonRead,
                ApiKeyScope::PlayersColonReadDetails,
            ])
        ));
        assert!(is_visible_to(&event, &api_key(&[ApiKeyScope::Star])));
    }

    #[test]
    fn friend_request_event_matches_the_openapi_payload() {
        let event = event();
        let serialized = serde_json::to_string(&event).expect("event must serialize");
        let payload: serde_json::Value =
            serde_json::from_str(&serialized).expect("event JSON must deserialize");
        let round_trip: StreamEvent =
            serde_json::from_str(&serialized).expect("generated event must deserialize");

        assert_eq!(payload["type"], "friend-request-added");
        assert_eq!(payload["data"]["sender"]["username"], "Sender");
        assert_eq!(payload["data"]["receiver"]["username"], "Receiver");
        assert_eq!(stream_event_type(&round_trip), Some("friend-request-added"));
    }

    fn event() -> StreamEvent {
        friend_request_added_event(
            player(uuid::Uuid::from_u128(1), "Sender"),
            player(uuid::Uuid::from_u128(2), "Receiver"),
        )
    }

    fn player(id: uuid::Uuid, username: &str) -> Player {
        Player::new(
            id,
            Nullable::Null,
            username.to_string(),
            "offline".to_string(),
            Nullable::Null,
            Nullable::Null,
        )
    }

    fn api_key(scopes: &[ApiKeyScope]) -> ApiKey {
        ApiKey::new(
            "Test API key".to_string(),
            "test-public-id".to_string(),
            scopes.iter().map(ToString::to_string).collect(),
            Utc::now(),
            Nullable::Null,
            Nullable::Null,
        )
    }
}

use crate::api::Api;
use crate::auth::scope::ApiKeyScopeExt;
use async_trait::async_trait;
use axum::response::sse::Event;
use axum_extra::extract::CookieJar;
use graph_api::apis::stream::{Stream, StreamEventsResponse};
use graph_api::models::*;
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
const REDIS_RECONNECT_MIN_DELAY: Duration = Duration::from_secs(1);
const REDIS_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(30);
const REDIS_PUBLISH_RETRY_DELAY: Duration = Duration::from_millis(250);

pub(crate) fn crawl_created_event(crawl: Crawl) -> StreamEvent {
    CrawlCreatedEvent::new(CrawlCreatedEventData::new(crawl)).into()
}

pub(crate) fn crawl_deleted_event(crawl: Crawl) -> StreamEvent {
    CrawlDeletedEvent::new(CrawlDeletedEventData::new(crawl)).into()
}

pub(crate) fn friend_added_event(player: Player, friend: Player) -> StreamEvent {
    FriendAddedEvent::new(FriendAddedEventData::new(player, friend)).into()
}

pub(crate) fn friend_removed_event(player: Player, friend: Player) -> StreamEvent {
    FriendRemovedEvent::new(FriendRemovedEventData::new(player, friend)).into()
}

pub(crate) fn friend_request_accepted_event(sender: Player, receiver: Player) -> StreamEvent {
    FriendRequestAcceptedEvent::new(FriendRequestAcceptedEventData::new(sender, receiver)).into()
}

pub(crate) fn friend_request_added_event(sender: Player, receiver: Player) -> StreamEvent {
    FriendRequestAddedEvent::new(FriendRequestAddedEventData::new(sender, receiver)).into()
}

pub(crate) fn friend_request_rejected_event(sender: Player, receiver: Player) -> StreamEvent {
    FriendRequestRejectedEvent::new(FriendRequestRejectedEventData::new(sender, receiver)).into()
}

pub(crate) fn friend_request_removed_event(sender: Player, receiver: Player) -> StreamEvent {
    FriendRequestRemovedEvent::new(FriendRequestRemovedEventData::new(sender, receiver)).into()
}

pub(crate) fn patch_note_created_event(patch_note: PatchNote) -> StreamEvent {
    PatchNoteCreatedEvent::new(PatchNoteCreatedEventData::new(patch_note)).into()
}

pub(crate) fn patch_note_deleted_event(patch_note: PatchNote) -> StreamEvent {
    PatchNoteDeletedEvent::new(PatchNoteDeletedEventData::new(patch_note)).into()
}

pub(crate) fn punishment_created_event(punishment: Punishment) -> StreamEvent {
    PunishmentCreatedEvent::new(PunishmentCreatedEventData::new(punishment)).into()
}

pub(crate) fn punishment_updated_event(punishment: Punishment) -> StreamEvent {
    PunishmentUpdatedEvent::new(PunishmentUpdatedEventData::new(punishment)).into()
}

pub(crate) fn punishment_revoked_event(punishment: Punishment) -> StreamEvent {
    PunishmentRevokedEvent::new(PunishmentRevokedEventData::new(punishment)).into()
}

pub(crate) fn punishment_proof_created_event(punishment_id: u64, proof: Proof) -> StreamEvent {
    PunishmentProofCreatedEvent::new(PunishmentProofCreatedEventData::new(punishment_id, proof))
        .into()
}

pub(crate) fn punishment_proof_updated_event(punishment_id: u64, proof: Proof) -> StreamEvent {
    PunishmentProofUpdatedEvent::new(PunishmentProofUpdatedEventData::new(punishment_id, proof))
        .into()
}

pub(crate) fn punishment_proof_deleted_event(punishment_id: u64, proof: Proof) -> StreamEvent {
    PunishmentProofDeletedEvent::new(PunishmentProofDeletedEventData::new(punishment_id, proof))
        .into()
}

fn stream_event_type(event: &StreamEvent) -> &str {
    let value = match event {
        StreamEvent::CrawlCreatedEvent(event) => &event.r_type,
        StreamEvent::CrawlDeletedEvent(event) => &event.r_type,
        StreamEvent::FriendAddedEvent(event) => &event.r_type,
        StreamEvent::FriendRemovedEvent(event) => &event.r_type,
        StreamEvent::FriendRequestAcceptedEvent(event) => &event.r_type,
        StreamEvent::FriendRequestAddedEvent(event) => &event.r_type,
        StreamEvent::FriendRequestRejectedEvent(event) => &event.r_type,
        StreamEvent::FriendRequestRemovedEvent(event) => &event.r_type,
        StreamEvent::PatchNoteCreatedEvent(event) => &event.r_type,
        StreamEvent::PatchNoteDeletedEvent(event) => &event.r_type,
        StreamEvent::PunishmentCreatedEvent(event) => &event.r_type,
        StreamEvent::PunishmentProofCreatedEvent(event) => &event.r_type,
        StreamEvent::PunishmentProofDeletedEvent(event) => &event.r_type,
        StreamEvent::PunishmentProofUpdatedEvent(event) => &event.r_type,
        StreamEvent::PunishmentRevokedEvent(event) => &event.r_type,
        StreamEvent::PunishmentUpdatedEvent(event) => &event.r_type,
    };
    value
}

fn is_visible_to(event: &StreamEvent, api_key: &ApiKey) -> bool {
    match event {
        StreamEvent::CrawlCreatedEvent(_) | StreamEvent::CrawlDeletedEvent(_) => {
            api_key.has_scope(&ApiKeyScope::CrawlsColonRead)
        }
        StreamEvent::FriendAddedEvent(_)
        | StreamEvent::FriendRemovedEvent(_)
        | StreamEvent::FriendRequestAcceptedEvent(_)
        | StreamEvent::FriendRequestAddedEvent(_)
        | StreamEvent::FriendRequestRejectedEvent(_)
        | StreamEvent::FriendRequestRemovedEvent(_) => {
            api_key.has_scope(&ApiKeyScope::PlayersColonRead)
                || api_key.has_scope(&ApiKeyScope::PlayersColonReadDetails)
        }
        StreamEvent::PatchNoteCreatedEvent(_) | StreamEvent::PatchNoteDeletedEvent(_) => {
            api_key.has_scope(&ApiKeyScope::PatchNotesColonRead)
        }
        StreamEvent::PunishmentCreatedEvent(_)
        | StreamEvent::PunishmentProofCreatedEvent(_)
        | StreamEvent::PunishmentProofDeletedEvent(_)
        | StreamEvent::PunishmentProofUpdatedEvent(_)
        | StreamEvent::PunishmentRevokedEvent(_)
        | StreamEvent::PunishmentUpdatedEvent(_) => {
            api_key.has_scope(&ApiKeyScope::PunishmentsColonRead)
        }
    }
}

fn into_sse(event: StreamEvent) -> Result<Event, axum::Error> {
    let event_type = stream_event_type(&event);
    Event::default().event(event_type).json_data(event)
}

impl Api {
    pub(crate) async fn publish_stream_event(&self, event: StreamEvent) {
        let Some(redis) = &self.redis_publisher else {
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
        let result = publish_to_redis(&mut redis, &payload).await;
        match result {
            Ok(0) => {
                tracing::warn!(
                    channel = STREAM_EVENTS_CHANNEL,
                    "stream event had no Redis subscribers; delivering locally"
                );
                let _ = self.stream_events.send(event);
            }
            Ok(_) => {}
            Err(error) => {
                tracing::error!(
                    %error,
                    channel = STREAM_EVENTS_CHANNEL,
                    "failed to publish stream event to Redis; delivering locally"
                );
                let _ = self.stream_events.send(event);
            }
        }
    }

    pub(crate) fn start_stream_event_listener(&self, redis_client: redis::Client) {
        let stream_events = self.stream_events.clone();
        tokio::spawn(listen_to_stream_events(redis_client, stream_events));
    }
}

async fn publish_to_redis(
    redis: &mut redis::aio::ConnectionManager,
    payload: &str,
) -> redis::RedisResult<usize> {
    match redis
        .publish::<_, _, usize>(STREAM_EVENTS_CHANNEL, payload)
        .await
    {
        Err(error) if error.is_connection_dropped() => {
            tracing::warn!(
                %error,
                channel = STREAM_EVENTS_CHANNEL,
                "failed to publish stream event to Redis; retrying after reconnect"
            );
            tokio::time::sleep(REDIS_PUBLISH_RETRY_DELAY).await;
            redis
                .publish::<_, _, usize>(STREAM_EVENTS_CHANNEL, payload)
                .await
        }
        result => result,
    }
}

async fn listen_to_stream_events(
    redis_client: redis::Client,
    stream_events: broadcast::Sender<StreamEvent>,
) {
    let mut reconnect_delay = REDIS_RECONNECT_MIN_DELAY;
    loop {
        let pubsub = match subscribe_to_stream_events(&redis_client).await {
            Ok(pubsub) => {
                reconnect_delay = REDIS_RECONNECT_MIN_DELAY;
                pubsub
            }
            Err(error) => {
                tracing::error!(
                    %error,
                    retry_after_seconds = reconnect_delay.as_secs(),
                    "failed to subscribe to Redis stream events"
                );
                tokio::time::sleep(reconnect_delay).await;
                reconnect_delay = next_reconnect_delay(reconnect_delay);
                continue;
            }
        };
        match receive_stream_events(pubsub, &stream_events).await {
            Ok(()) => tracing::warn!(
                retry_after_seconds = reconnect_delay.as_secs(),
                "Redis stream event listener disconnected"
            ),
            Err(error) => tracing::error!(
                %error,
                retry_after_seconds = reconnect_delay.as_secs(),
                "Redis stream event listener disconnected"
            ),
        }
        tokio::time::sleep(reconnect_delay).await;
        reconnect_delay = next_reconnect_delay(reconnect_delay);
    }
}

fn next_reconnect_delay(current: Duration) -> Duration {
    current.saturating_mul(2).min(REDIS_RECONNECT_MAX_DELAY)
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
    fn redis_reconnect_delay_is_exponential_and_capped() {
        assert_eq!(
            next_reconnect_delay(REDIS_RECONNECT_MIN_DELAY),
            Duration::from_secs(2)
        );
        assert_eq!(
            next_reconnect_delay(Duration::from_secs(16)),
            REDIS_RECONNECT_MAX_DELAY
        );
        assert_eq!(
            next_reconnect_delay(REDIS_RECONNECT_MAX_DELAY),
            REDIS_RECONNECT_MAX_DELAY
        );
    }

    #[tokio::test]
    #[ignore = "requires a dedicated TEST_REDIS_URL because it kills Redis client connections"]
    async fn redis_connections_recover_after_being_killed() {
        let redis_url = std::env::var("TEST_REDIS_URL").expect("TEST_REDIS_URL must be set");
        let redis_client = redis::Client::open(redis_url).expect("test Redis URL must be valid");
        let publisher_config = redis::aio::ConnectionManagerConfig::new()
            .set_min_delay(Duration::from_millis(10))
            .set_max_delay(Duration::from_millis(50))
            .set_number_of_retries(5);
        let mut publisher =
            redis::aio::ConnectionManager::new_with_config(redis_client.clone(), publisher_config)
                .await
                .expect("publisher must connect");
        let mut admin = redis_client
            .get_multiplexed_async_connection()
            .await
            .expect("admin connection must connect");
        let (stream_events, mut received_events) = broadcast::channel(4);
        let listener = tokio::spawn(listen_to_stream_events(redis_client, stream_events));

        wait_for_redis_subscribers(&mut admin, 1).await;
        let payload = serde_json::to_string(&event()).expect("event must serialize");
        assert_eq!(
            publish_to_redis(&mut publisher, &payload)
                .await
                .expect("initial publish must succeed"),
            1
        );
        received_events
            .recv()
            .await
            .expect("initial event must be received");

        let killed_publishers: usize = redis::cmd("CLIENT")
            .arg("KILL")
            .arg("TYPE")
            .arg("normal")
            .arg("SKIPME")
            .arg("yes")
            .query_async(&mut admin)
            .await
            .expect("publisher connection must be killed");
        assert!(killed_publishers >= 1);
        assert_eq!(
            publish_to_redis(&mut publisher, &payload)
                .await
                .expect("publisher must reconnect"),
            1
        );
        received_events
            .recv()
            .await
            .expect("event after publisher reconnect must be received");

        let killed_subscribers: usize = redis::cmd("CLIENT")
            .arg("KILL")
            .arg("TYPE")
            .arg("pubsub")
            .query_async(&mut admin)
            .await
            .expect("subscriber connection must be killed");
        assert_eq!(killed_subscribers, 1);
        wait_for_redis_subscribers(&mut admin, 0).await;
        wait_for_redis_subscribers(&mut admin, 1).await;
        publish_to_redis(&mut publisher, &payload)
            .await
            .expect("publish after subscriber reconnect must succeed");
        received_events
            .recv()
            .await
            .expect("event after subscriber reconnect must be received");

        listener.abort();
    }

    async fn wait_for_redis_subscribers(
        redis: &mut redis::aio::MultiplexedConnection,
        expected: usize,
    ) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (_, subscribers): (String, usize) = redis::cmd("PUBSUB")
                    .arg("NUMSUB")
                    .arg(STREAM_EVENTS_CHANNEL)
                    .query_async(redis)
                    .await
                    .expect("subscriber count must be available");
                if subscribers == expected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("subscriber count must reach the expected value");
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
        assert_eq!(stream_event_type(&round_trip), "friend-request-added");
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

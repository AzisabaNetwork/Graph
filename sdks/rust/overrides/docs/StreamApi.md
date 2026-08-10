# Stream API

`stream_events` opens `/stream` as a Server-Sent Events connection and yields
the generated `StreamEvent` enum.

```rust
use futures_util::StreamExt;

let mut configuration = azisaba_graph::apis::configuration::Configuration::new();
configuration.bearer_access_token = Some(api_key);

let events = azisaba_graph::apis::stream_api::stream_events(&configuration).await?;
tokio::pin!(events);
while let Some(event) = events.next().await {
    println!("{:?}", event?);
}
```

Drop the returned stream to disconnect. The SDK does not reconnect
automatically.

# StreamApi

`StreamApi` opens `/stream` as a Server-Sent Events connection and exposes the
generated `StreamEvent` model as a closeable Java stream.

```java
ApiClient client = new ApiClient().setRequestInterceptor(builder ->
    builder.header("Authorization", "Bearer " + apiKey));
StreamApi api = new StreamApi(client);

try (Stream<StreamEvent> events = api.streamEvents().join()) {
  events.forEach(event -> System.out.println(event.getActualInstance()));
}
```

Close the returned stream to disconnect. The SDK does not reconnect
automatically.

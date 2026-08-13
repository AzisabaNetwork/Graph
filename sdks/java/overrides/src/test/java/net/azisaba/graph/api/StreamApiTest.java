package net.azisaba.graph.api;

import com.fasterxml.jackson.databind.ObjectMapper;
import net.azisaba.graph.ApiClient;
import net.azisaba.graph.model.FriendRemovedEvent;
import net.azisaba.graph.model.StreamEvent;
import org.junit.jupiter.api.Test;

import java.io.IOException;

import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

class StreamApiTest {
  private static final String PLAYER = "{"
      + "\"id\":\"00000000-0000-0000-0000-000000000001\","
      + "\"discordId\":null,"
      + "\"username\":\"player\","
      + "\"bio\":null,"
      + "\"status\":\"offline\","
      + "\"currentServer\":null,"
      + "\"currentLocale\": null,",
      + "\"currentClientVersion\": null"
      + "}";

  private final ObjectMapper objectMapper = ApiClient.createDefaultObjectMapper();

  @Test
  void dispatchesStructurallyIdenticalEventsByType() throws IOException {
    String payload = String.format(
        "{\"type\":\"friend-removed\",\"data\":{\"player\":%s,\"friend\":%s}}",
        PLAYER,
        PLAYER);

    StreamEvent event = objectMapper.readValue(payload, StreamEvent.class);

    assertInstanceOf(FriendRemovedEvent.class, event.getActualInstance());
  }

  @Test
  void rejectsUnknownEventTypes() {
    IOException error = assertThrows(
        IOException.class,
        () -> objectMapper.readValue("{\"type\":\"unknown\",\"data\":{}}", StreamEvent.class));

    assertTrue(error.getMessage().contains("Failed deserialization for StreamEvent"));
  }
}

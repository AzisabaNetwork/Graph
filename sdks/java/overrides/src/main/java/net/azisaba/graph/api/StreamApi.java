package net.azisaba.graph.api;

import com.fasterxml.jackson.databind.ObjectMapper;
import net.azisaba.graph.ApiClient;
import net.azisaba.graph.ApiException;
import net.azisaba.graph.Configuration;
import net.azisaba.graph.model.StreamEvent;

import java.io.IOException;
import java.io.UncheckedIOException;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.util.Iterator;
import java.util.Map;
import java.util.Spliterator;
import java.util.Spliterators.AbstractSpliterator;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionException;
import java.util.function.Consumer;
import java.util.stream.Stream;
import java.util.stream.Collectors;
import java.util.stream.StreamSupport;

/**
 * Streams Graph events using Server-Sent Events.
 *
 * <p>This class is maintained separately from the generated API implementation
 * because OpenAPI Generator does not expose {@code text/event-stream} as a
 * sequence of typed events for the Java native HTTP client.</p>
 */
public class StreamApi {
  private final HttpClient httpClient;
  private final ObjectMapper objectMapper;
  private final String baseUri;
  private final Consumer<HttpRequest.Builder> requestInterceptor;

  public StreamApi() {
    this(Configuration.getDefaultApiClient());
  }

  public StreamApi(ApiClient apiClient) {
    httpClient = apiClient.getHttpClient();
    objectMapper = apiClient.getObjectMapper();
    baseUri = apiClient.getBaseUri();
    requestInterceptor = apiClient.getRequestInterceptor();
  }

  /**
   * Opens the event stream.
   *
   * <p>The returned stream must be closed by the caller. Closing it releases
   * the underlying HTTP response body and disconnects from the endpoint.</p>
   *
   * @return a future that completes with the stream after response headers arrive
   */
  public CompletableFuture<Stream<StreamEvent>> streamEvents() {
    return streamEvents(null);
  }

  /**
   * Opens the event stream with additional request headers.
   *
   * @param headers headers to add to the request, or {@code null}
   * @return a future that completes with the stream after response headers arrive
   */
  public CompletableFuture<Stream<StreamEvent>> streamEvents(Map<String, String> headers) {
    HttpRequest.Builder requestBuilder = HttpRequest.newBuilder()
        .uri(URI.create(baseUri + "/stream"))
        .header("Accept", "text/event-stream")
        .GET();

    if (headers != null) {
      headers.forEach(requestBuilder::header);
    }
    if (requestInterceptor != null) {
      requestInterceptor.accept(requestBuilder);
    }

    return httpClient.sendAsync(requestBuilder.build(), HttpResponse.BodyHandlers.ofLines())
        .thenApply(this::toEventStream);
  }

  private Stream<StreamEvent> toEventStream(HttpResponse<Stream<String>> response) {
    Stream<String> lines = response.body();
    if (response.statusCode() < 200 || response.statusCode() >= 300) {
      String body;
      try (lines) {
        body = lines.collect(Collectors.joining("\n"));
      }
      throw new CompletionException(new ApiException(
          response.statusCode(),
          "streamEvents call failed with: " + response.statusCode() + " - " + body,
          response.headers(),
          body));
    }

    String contentType = response.headers().firstValue("Content-Type").orElse("");
    if (!contentType.toLowerCase().startsWith("text/event-stream")) {
      lines.close();
      throw new CompletionException(new ApiException(
          response.statusCode(),
          "streamEvents returned an unexpected Content-Type: " + contentType,
          response.headers(),
          null));
    }

    EventSpliterator spliterator = new EventSpliterator(lines.iterator(), objectMapper, lines);
    return StreamSupport.stream(spliterator, false).onClose(lines::close);
  }

  private static final class EventSpliterator extends AbstractSpliterator<StreamEvent> {
    private final Iterator<String> lines;
    private final ObjectMapper objectMapper;
    private final Stream<String> responseBody;
    private final StringBuilder data = new StringBuilder();
    private boolean hasData;

    private EventSpliterator(
        Iterator<String> lines,
        ObjectMapper objectMapper,
        Stream<String> responseBody) {
      super(Long.MAX_VALUE, Spliterator.ORDERED | Spliterator.NONNULL);
      this.lines = lines;
      this.objectMapper = objectMapper;
      this.responseBody = responseBody;
    }

    @Override
    public boolean tryAdvance(Consumer<? super StreamEvent> action) {
      while (lines.hasNext()) {
        String line = lines.next();
        if (line.isEmpty()) {
          if (dispatch(action)) {
            return true;
          }
          continue;
        }
        if (line.startsWith(":")) {
          continue;
        }

        int separator = line.indexOf(':');
        String field = separator == -1 ? line : line.substring(0, separator);
        String value = separator == -1 ? "" : line.substring(separator + 1);
        if (value.startsWith(" ")) {
          value = value.substring(1);
        }
        if (field.equals("data")) {
          if (hasData) {
            data.append('\n');
          }
          data.append(value);
          hasData = true;
        }
      }

      boolean dispatched = dispatch(action);
      responseBody.close();
      return dispatched;
    }

    private boolean dispatch(Consumer<? super StreamEvent> action) {
      if (!hasData) {
        return false;
      }
      String payload = data.toString();
      data.setLength(0);
      hasData = false;
      try {
        action.accept(objectMapper.readValue(payload, StreamEvent.class));
        return true;
      } catch (IOException exception) {
        throw new UncheckedIOException("Could not deserialize a stream event.", exception);
      }
    }
  }
}

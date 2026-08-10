# StreamApi

`StreamApi` opens `/stream` as a Server-Sent Events connection and exposes the
generated `StreamEvent` union through an async iterator.

```ts
import { Configuration, StreamApi } from '@azisaba/graph';

const controller = new AbortController();
const api = new StreamApi(new Configuration({ accessToken: 'api-key' }));

for await (const event of api.streamEvents({ signal: controller.signal })) {
  console.log(event.type, event.data);
}
```

Abort the supplied signal or stop iteration to close the connection. The SDK
does not reconnect automatically.

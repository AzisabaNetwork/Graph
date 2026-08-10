/* tslint:disable */
/* eslint-disable */

import * as runtime from '../runtime';
import {
    type StreamEvent,
    StreamEventFromJSON,
} from '../models/StreamEvent';

/**
 * StreamApi - interface
 *
 * This file is maintained separately from the generated API implementation
 * because OpenAPI Generator does not expose text/event-stream as an async
 * sequence for TypeScript clients.
 */
export interface StreamApiInterface {
    /** Opens the event stream and returns its raw response. */
    streamEventsRaw(initOverrides?: RequestInit | runtime.InitOverrideFunction): Promise<Response>;

    /** Streams and deserializes events until the connection closes or is aborted. */
    streamEvents(initOverrides?: RequestInit | runtime.InitOverrideFunction): AsyncGenerator<StreamEvent, void, unknown>;
}

export class StreamApi extends runtime.BaseAPI implements StreamApiInterface {
    async streamEventsRaw(initOverrides?: RequestInit | runtime.InitOverrideFunction): Promise<Response> {
        const headerParameters: runtime.HTTPHeaders = {
            Accept: 'text/event-stream',
        };

        if (this.configuration && this.configuration.accessToken) {
            const token = await this.configuration.accessToken('apiKeyAuth', ['stream:read']);
            if (token) {
                headerParameters.Authorization = `Bearer ${token}`;
            }
        }

        return this.request({
            path: '/stream',
            method: 'GET',
            headers: headerParameters,
            query: {},
        }, initOverrides);
    }

    async *streamEvents(initOverrides?: RequestInit | runtime.InitOverrideFunction): AsyncGenerator<StreamEvent, void, unknown> {
        const response = await this.streamEventsRaw(initOverrides);
        if (!response.body) {
            throw new Error('The response does not contain a readable event stream.');
        }

        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        const parser = new EventStreamParser();

        try {
            while (true) {
                const { done, value } = await reader.read();
                const payloads = parser.push(
                    decoder.decode(value, { stream: !done }),
                    done,
                );

                for (const payload of payloads) {
                    yield StreamEventFromJSON(JSON.parse(payload));
                }

                if (done) {
                    break;
                }
            }
        } finally {
            await reader.cancel().catch(() => undefined);
            reader.releaseLock();
        }
    }
}

class EventStreamParser {
    private buffer = '';
    private data: string[] = [];

    push(chunk: string, endOfStream: boolean): string[] {
        this.buffer += chunk;
        const payloads: string[] = [];

        while (true) {
            const line = this.takeLine(endOfStream);
            if (line === undefined) {
                break;
            }

            const payload = this.consumeLine(line);
            if (payload !== undefined) {
                payloads.push(payload);
            }
        }

        if (endOfStream) {
            const payload = this.dispatch();
            if (payload !== undefined) {
                payloads.push(payload);
            }
        }

        return payloads;
    }

    private takeLine(endOfStream: boolean): string | undefined {
        for (let index = 0; index < this.buffer.length; index += 1) {
            const character = this.buffer[index];
            if (character === '\n') {
                const line = this.buffer.slice(0, index);
                this.buffer = this.buffer.slice(index + 1);
                return line;
            }
            if (character === '\r') {
                if (index + 1 === this.buffer.length && !endOfStream) {
                    return undefined;
                }
                const line = this.buffer.slice(0, index);
                const length = this.buffer[index + 1] === '\n' ? index + 2 : index + 1;
                this.buffer = this.buffer.slice(length);
                return line;
            }
        }

        if (endOfStream && this.buffer.length > 0) {
            const line = this.buffer;
            this.buffer = '';
            return line;
        }
        return undefined;
    }

    private consumeLine(line: string): string | undefined {
        if (line.length === 0) {
            return this.dispatch();
        }
        if (line.startsWith(':')) {
            return undefined;
        }

        const separator = line.indexOf(':');
        const field = separator === -1 ? line : line.slice(0, separator);
        let value = separator === -1 ? '' : line.slice(separator + 1);
        if (value.startsWith(' ')) {
            value = value.slice(1);
        }
        if (field === 'data') {
            this.data.push(value);
        }
        return undefined;
    }

    private dispatch(): string | undefined {
        if (this.data.length === 0) {
            return undefined;
        }
        const payload = this.data.join('\n');
        this.data = [];
        return payload;
    }
}

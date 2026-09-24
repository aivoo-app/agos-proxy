# AGOS Proxy API Reference

> OpenAI-compatible HTTP surface served by AGOS Proxy. This documents the
> endpoints, authentication, error shape, rate limiting, and streaming behavior.

## Base URL and version

The proxy exposes an OpenAI-compatible API at the bind address you configure:

```
http(s)://<host>:<port>/v1/...
```

There is no API version negotiation today. The surface is OpenAI-compatible and
stable within the bounds of that compatibility.

## Authentication

All requests must include a bearer token:

```
Authorization: Bearer <profile-api-token>
```

The token is the profile's opaque `id`, generated at profile creation and
printed once. It grants access to that profile's routes and providers.

Example:

```sh
curl -H "Authorization: Bearer sk-profile-..." \
     -H "Content-Type: application/json" \
     http://localhost:8080/v1/models
```

Without a valid token, the proxy returns `401 Unauthorized`.

### Request tracking

Every request receives a stable `X-Request-ID`. A caller-supplied ID is reused;
otherwise AGOS generates a UUID. The ID is echoed on the response, forwarded to
each upstream attempt, and stored on every usage row so `agos-proxy usage recent`
can correlate failover attempts with the caller's logs. It is metadata only and
is never inserted into a provider's JSON payload.

If the token is valid but the profile has been deleted or is otherwise not
usable, the proxy returns `401` or `403` as appropriate.

## Endpoints

### `GET /v1/models`

Lists the models the authenticated profile may call.

Response shape:

```json
{
  "object": "list",
  "data": [
    {
      "id": "Programmer/php-developer-3.5-flash",
      "object": "model",
      "created": 1700000000,
      "owned_by": "agos"
    }
  ]
}
```

Each entry corresponds to a route the profile can reach. The `id` is the
`<proxy>/<route>` string the caller sends in the `model` field of completion
requests.

Notes:

- Only routes belonging to the authenticated profile are listed.
- The `created` timestamp is the route's creation time (or a representative
  value, depending on the store).
- `owned_by` is `agos` for all AGOS-managed routes.

### `POST /v1/chat/completions`

The primary endpoint. Accepts an OpenAI-style chat completion request and
returns an OpenAI-style completion (or SSE stream).

Request body (OpenAI-compatible):

```json
{
  "model": "Programmer/php-developer-3.5-flash",
  "messages": [
    { "role": "system", "content": "You are helpful." },
    { "role": "user", "content": "Hello." }
  ],
  "stream": false,
  "max_tokens": 1024,
  "temperature": 1.0,
  "top_p": 1.0,
  "stop": ["END"],
  "n": 1
}
```

`messages` may contain ordered `text`, `image_url`, `input_audio`,
`audio_url`, `video_url`, and `file` / `input_file` parts. `tools`,
`tool_choice`, assistant `tool_calls`, and `tool` messages are forwarded to
compatible upstreams and translated to the selected provider's native shape.
Requests are never silently flattened when a destination cannot represent a
feature: the route skips that entry and tries another capability-compatible one.

Response shape (non-streaming):

```json
{
  "id": "chatcmpl-<unique>",
  "object": "chat.completion",
  "created": 1700000000,
  "model": "Programmer/php-developer-3.5-flash",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "Hello! How can I help you?"
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 10,
    "completion_tokens": 8,
    "total_tokens": 18
  }
  ...
}
```

Response shape (streaming): OpenAI-compatible SSE stream.

The proxy resolves `model` to a route, resolves the route's chain to a healthy
entry, and forwards the request. On failure, the next entry is tried (non-
streaming) or the stream may break (streaming — see below).

### `POST /v1/completions`

Text completion endpoint (OpenAI-compatible).

Request body (OpenAI-compatible):

```json
{
  "model": "Programmer/php-developer-3.5-flash",
  "prompt": "Once upon a time",
  "max_tokens": 50,
  "temperature": 1.0
}
```

Response shape:

```json
{
  "id": "cmpl-<unique>",
  "object": "text_completion",
  "created": 1700000000,
  "model": "Programmer/php-developer-3.5-flash",
  "choices": [
    {
      "text": " there lived a...",
      "index": 0,
      "logprobs": null,
      "finish_reason": "length"
    }
  ],
  "usage": {
    "prompt_tokens": 4,
    "completion_tokens": 5,
    "total_tokens": 9
  }
}
```

### `POST /v1/embeddings`

Embedding endpoint (OpenAI-compatible). Used by profiles that have a route entry
configured on a provider that supports embeddings.

Request body:

```json
{
  "model": "Programmer/embedding-route",
  "input": "text to embed"
}
```

Response shape:

```json
{
  "object": "list",
  "model": "Programmer/embedding-route",
  "data": [
    {
      "object": "embedding",
      "index": 0,
      "embedding": [0.1, 0.2, 0.3, ...]
    }
  ],
  "usage": {
    "prompt_tokens": 4,
    "total_tokens": 4
  }
}
```

### `GET /health`

Liveness probe. Returns `200 OK` with a small JSON body when the proxy is alive.
Use this for load balancer or orchestrator health checks.

Response shape:

```json
{
  "status": "ok"
}
```

### Non-standard extensions

AGOS Proxy passes through most of the OpenAI request body as-is. The following
fields are supported for routing purposes where applicable:

| Field            | Use                                                          |
|------------------|--------------------------------------------------------------|
| `model`          | Required. Resolved to a route in `<proxy>/<route>` form.    |
| `stream`         | Streaming flag.                                              |
| `messages`       | Chat messages.                                               |
| `max_tokens`     | Forwarded to the upstream.                                   |
| `temperature`    | Forwarded to the upstream.                                   |
| `top_p`          | Forwarded to the upstream.                                   |
| `stop`           | Forwarded to the upstream.                                   |
| `n`              | Number of completions (where supported).                    |
| `tools` / `tool_choice` | Forwarded where the upstream and translator support it. |

Provider-specific fields beyond this list are forwarded if they are part of the
OpenAI-compatible surface the upstream expects.

## Error responses

Errors use the OpenAI error envelope where possible:

```json
{
  "error": {
    "message": "Human-readable message",
    "type": "error_type",
    "param": null,
    "code": "error_code"
  }
}
```

HTTP status codes used:

| Status  | Meaning                                                               |
|---------|-----------------------------------------------------------------------|
| 400     | Malformed request (bad body, missing required fields, etc.).         |
| 401     | Missing or invalid bearer token.                                      |
| 403     | Token valid, but profile not usable for the request.                  |
| 404     | Model not found (no route matches the `model` string).                |
| 429     | Rate limit hit (per-profile sliding window).                          |
| 500     | Internal error in the proxy itself (not a provider error).            |
| 502     | Upstream returned an error that could not be mapped.                  |
| 503     | No healthy provider in the route chain.                                |

Provider errors (from the upstream) are forwarded when possible so the caller can
see *why* a particular provider failed. When failover succeeds, the caller sees
only the final successful response.

## Streaming behavior

When `stream: true`, the response is an SSE stream of OpenAI-compatible chunks.

Non-streaming failover is transparent: if the first provider fails, the next is
tried before the response is returned. Streaming failover has the documented
limitation described in `docs/failover.md`: if the stream fails after tokens have
begun flowing, the partial failure is logged and the stream breaks.

Chunk shape (OpenAI-compatible):

```json
{
  "id": "chatcmpl-<unique>",
  "object": "chat.completion.chunk",
  "created": 1700000000,
  "model": "Programmer/php-developer-3.5-flash",
  "choices": [
    {
      "index": 0,
      "delta": {
        "role": "assistant"
      },
      "finish_reason": null
    }
  ]
}
```

The final chunk has `finish_reason` set to `stop`, `length`, `content_filter`,
or `null` as appropriate.

## Rate limiting

Per-profile rate limiting is enforced with a sliding one-minute window. If a
profile has an `rpm_limit` set, the proxy returns `429` once the profile has made
more than that many requests in the last 60 seconds.

Rate limiting is local to a single proxy process. Multiple instances do not
coordinate; for coordinated rate limiting, place a rate-limiting reverse proxy in
front.

## Model resolution

The `model` string is resolved as `<proxy-name>/<route-name>`. For example,
`Programmer/php-developer-3.5-flash` resolves to the route named
`php-developer-3.5-flash` under the proxy named `Programmer`, in the
authenticated profile.

Resolution is scoped to the authenticated profile. There is no cross-profile
model access.

## Request context flow

1. `Authorization` header → profile.
2. `X-Request-ID` → stable request correlation id (generated when absent).
3. `model` field → route within that profile.
4. Request body → translated to the chosen provider's shape.
5. Provider response → passed back to the caller (optionally translated back).
6. Usage and request id logged against the route entry that handled (or tried) the request.

## Error and failure visibility

- Caller-visible errors: HTTP status + OpenAI error envelope.
- Proxy-internal failures: logged; may surface as 5xx.
- Provider failures that trigger failover: invisible to the caller (non-streaming)
  or break the stream (streaming), but logged in the usage log.
- Provider failures that exhaust the chain: surfaced as an error to the caller.

Use the usage tooling (`atos-proxy usage stats`, `atos-proxy usage recent`) to
inspect what happened after the fact.

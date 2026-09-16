# Providers

What provider kinds AGOS Proxy supports, how the request/response translation
works, and notes for setting up common providers.

## Provider kinds

AGOS Proxy classifies providers by the wire protocol they speak. The kind
determines how AGOS Proxy shapes the request before sending it and how it
interprets the response after receiving it.

| Kind                  | Protocol                     | Notes                                              |
|-----------------------|------------------------------|-----------------------------------------------------|
| `OpenAICompatible`  | OpenAI REST (pass-through)   | Most providers: DeepSeek, OpenRouter, Ollama, etc. |
| `OpenAIResponses`   | OpenAI Responses (`/v1/responses`) | Responses-only models (e.g. Zen muse-* free tiers). |
| `Anthropic`          | Anthropic `/v1/messages`     | Native translation of chat completions.            |
| `Google`             | Gemini `generateContent`     | Native translation of chat completions.            |
| `Custom`             | Reserved for future use      | Not implemented yet.                               |

The kind is set when you add a provider and is used to pick the right
translator at request time.

## Provider model naming

AGOS Proxy does not enforce any provider naming convention. You decide:

- The provider `name` is a label you choose (e.g. `deepseek`, `claude`,
  `google`).
- The model `id` is whatever string the provider expects (e.g. `deepseek-chat`,
  `claude-sonnet-4-5`, `gemini-2.5-pro-exp`).

These are the values you configure in route entries. AGOS Proxy does not try to
validate them against a registry — it sends them verbatim to the provider.

## OpenAI-compatible providers

Most providers speak an OpenAI-compatible API. For these, AGOS Proxy forwards
the request mostly as-is and parses the response as an OpenAI-style completion.

Supported paths:

| Path                          | Notes                                              |
|-------------------------------|----------------------------------------------------|
| `POST /v1/chat/completions`   | Streaming and non-streaming.                       |
| `POST /v1/completions`        | Text completion.                                   |
| `POST /v1/embeddings`         | Embedding vectors.                                 |
| `GET  /v1/models`             | Auto-listed from routes in non-streaming contexts. |

This covers the bulk of providers you are likely to configure: DeepSeek,
OpenRouter, Together AI, Ollama, local servers, and any other OpenAI-compatible
endpoint.

### Extra headers

Some providers need extra headers beyond `Authorization`. You can configure them
per provider:

```sh
atos-proxy provider edit --profile coder1 --name deepseek
# then add headers when prompted, or use the flag-driven path
```

Headers are sent verbatim with every request to that provider.

### Setting up DeepSeek

```sh
atos-proxy provider add --profile coder1
# name: deepseek
# base_url: https://api.deepseek.com
# auth_token: sk-... (your DeepSeek API key)
# kind: openai
```

Route entry:

```json
{ "provider": "deepseek", "model": "deepseek-chat" }
```

### Setting up OpenRouter

```sh
atos-proxy provider add --profile coder1
# name: openrouter
# base_url: https://openrouter.ai/api/v1
# auth_token: sk-or-v1-...
# kind: openai
```

Route entry:

```json
{ "provider": "openrouter", "model": "openai/gpt-4o" }
```

### Setting up a local / self-hosted server (Ollama, lmstudio, etc.)

```sh
atos-proxy provider add --profile coder1
# name: ollama
# base_url: http://localhost:11434/v1
# auth_token: (empty if no auth)
# kind: openai
```

Route entry:

```json
{ "provider": "ollama", "model": "llama3" }
```

### OpenAI itself

OpenAI is just an OpenAI-compatible provider:

```sh
atos-proxy provider add --profile coder1
# name: openai
# base_url: https://api.openai.com/v1
# auth_token: sk-...
# kind: openai
```

Route entry:

```json
{ "provider": "openai", "model": "gpt-4o" }
```

## Anthropic

Anthropic's API uses `/v1/messages` with a different body shape than OpenAI.
AGOS Proxy translates between the OpenAI-style chat completion body it receives
from callers and the Anthropic `/v1/messages` shape it sends upstream.

Supported paths via the Anthropic translator:

| Path                     | Notes                                        |
|--------------------------|----------------------------------------------|
| `POST /v1/messages`      | Chat completions (streaming and non-streaming). |

Anthropic does not expose `/v1/completions`, `/v1/embeddings`, or
`/v1/models` in the same shape as OpenAI. The translator handles what is
supported; unsupported paths are rejected appropriately.

### Setting up Anthropic

```sh
atos-proxy provider add --profile coder1
# name: claude
# base_url: https://api.anthropic.com
# auth_token: sk-ant-...
# kind: anthropic
```

Route entry:

```json
{ "provider": "claude", "model": "claude-sonnet-4-5" }
```

### Anthropic-specific notes

- Anthropic bills by input/output tokens differently from OpenAI. AGOS Proxy
  logs what the provider reports; the numbers are what Anthropic returns.
- System prompts are mapped into the `messages` array appropriately for
  Anthropic.
- Tool/function calling is supported through the translator where the provider
  supports it.
- **Vision:** image parts in message content are forwarded as Anthropic `image`
  blocks — `source.type = "url"` for `http(s)` references and
  `source.type = "base64"` for `data:` URLs. Images inside `system` prompts are
  dropped (Anthropic system prompts are text-only).

## Google Gemini

Gemini uses `generateContent` (and `streamGenerateContent`) rather than the
OpenAI chat completions shape. AGOS Proxy translates the OpenAI-style body it
receives from callers into the Gemini request shape.

Supported paths via the Google translator:

| Path                          | Notes                                        |
|-------------------------------|----------------------------------------------|
| `POST /v1/.../generateContent`| Chat completions (streaming and non-streaming). |

### Setting up Google AI Studio

```sh
atos-proxy provider add --profile coder1
# name: google
# base_url: https://generativelanguage.googleapis.com/v1beta
# auth_token: AIza...
# kind: google
```

Route entry:

```json
{ "provider": "google", "model": "gemini-2.5-pro-exp" }
```

### Google-specific notes

- Google's API key is passed as a query parameter (`key=...`) by the translator
  when the base URL and provider kind indicate Google.
- Streaming is supported through the Gemini streaming endpoint.
- Token usage is reported as Google returns it.
- **Vision:** image parts in message content are forwarded as Gemini parts —
  `fileData` (`fileUri` + MIME type inferred from the URL's file extension,
  `image/jpeg` default) for `http(s)` references and `inlineData` for `data:`
  URLs. Images inside `systemInstruction` are dropped (Gemini system
  instructions are text-only).

## OpenAI Responses upstreams

Some models — notably the Zen `muse-*` free tiers — only serve the OpenAI
*Responses* endpoint (`POST {base}/v1/responses`) and reject
`/v1/chat/completions`. Create the provider with
`provider add --kind openai_responses` (same base URL, Bearer token and
repeatable `--header` support as `openai`).

How a chat request is served:

- The transcript is flattened into a single `input` string
  (`role: content` lines, in order).
- `temperature` and `top_p` are passed through; `max_tokens` /
  `max_completion_tokens` map to `max_output_tokens`.
- `tools`, `tool_choice` and `response_format` are dropped with a debug log
  (tool calling over Responses is a future feature).
- The reply's `output[]` items are walked for `message` entries and the
  `output_text`/`text` parts are joined into the assistant message. If the
  reply carries no assistant text (e.g. reasoning-only output), the attempt
  fails and the router falls over to the next chain entry.
- Token usage is reported only when the upstream sends `input_tokens` /
  `output_tokens`; nothing is fabricated.

**Streaming:** the Responses adapter serves `stream: true` requests from the
non-streamed answer, wrapped into a minimal OpenAI SSE stream (content chunk,
`stop` chunk, `[DONE]`) so standard OpenAI SDK clients keep working.

### Zen setup example

```console
$ agos-proxy provider add --profile coder1 --name zen \
    --base-url https://api.zen.ai \
    --auth-token sk-zen-... \
    --kind openai_responses
$ agos-proxy route entry add --proxy programmer --route php-dev \
    --provider zen --model muse-spark-x-contributor-free --priority 1
```

## Provider-specific headers and auth styles

Different providers authenticate differently:

| Provider        | Auth style                         | Configured as                        |
|-----------------|------------------------------------|--------------------------------------|
| OpenAI          | `Authorization: Bearer <key>`      | `auth_token` field.                  |
| DeepSeek        | `Authorization: Bearer <key>`      | `auth_token` field.                  |
| OpenRouter      | `Authorization: Bearer <key>`      | `auth_token` field.                  |
| Anthropic       | `x-api-key` + `anthropic-version`  | `auth_token` field + headers may be set. |
| Google          | `?key=...` query param             | `auth_token` field; translator adds it.   |
| Ollama/local    | Often none                         | Empty `auth_token`; rely on network isolation. |

If a provider has a non-standard auth mechanism that is not covered by the
`auth_token` field or extra headers, let us know — we can add support.

## Provider notes and limitations

- AGOS Proxy does not validate provider configuration against the provider's
  actual API at config time. It attempts the real call at request time. If a
  provider is misconfigured, you will see failures in the usage log and the
  route entry will be marked unhealthy.
- Rate limits are provider-specific. AGOS Proxy's own rate limiter is per-profile
  and separate from provider rate limits. If a provider returns 429, AGOS Proxy
  treats that as a failure for failover purposes and marks the entry unhealthy
  temporarily.
- Streaming passthrough depends on the provider supporting streaming. If a
  provider does not support streaming, mark that route entry's streaming
  capability accordingly (or just do not stream to it).
- Some providers return non-standard error shapes. AGOS Proxy parses the OpenAI-
  style error envelope where possible; provider-specific error translation is
  limited today.

## Adding a new provider kind

If you need native support for a provider that is not OpenAI-compatible,
Anthropic, or Google:

1. Add the kind to the `ProviderKind` enum in `domain/mod.rs`.
2. Add a translator module in `translator/` that converts an incoming
   OpenAI-compatible body to the provider's shape and back.
3. Wire the translator selection into the request path.
4. Add provider setup notes here.
5. Update the architecture and API docs.

Provider-specific native integrations are welcome as contributions. See
`CONTRIBUTING.md`.

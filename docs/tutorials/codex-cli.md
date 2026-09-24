# Using AGOS Proxy with the Codex CLI

The [OpenAI Codex CLI](https://developers.openai.com/codex/) can run against
AGOS Proxy through the OpenAI **Responses API** surface. AGOS translates
Responses requests into chat-completions calls for whichever provider is
upstream (OpenAI-compatible, Anthropic, or Gemini), and translates the reply
back — including function/tool calls, which is what Codex uses to run shell
commands, patch files, and read your project.

## 1. Enable tools on the route

Codex always sends tool definitions, so the route entry must advertise the
`tools` capability (the `agos route create` wizard prompts for it):

```
agos route add-entry --proxy programmer --route php-dev ... --tools
```

An entry without the tools flag is skipped during resolution and the request
fails over (or fails) even if the provider itself would work.

## 2. Point Codex at AGOS

Add a model provider to `~/.codex/config.toml`:

```toml
[model_providers.agos]
name = "AGOS"
base_url = "http://127.0.0.1:3000/codex/v1"
env_key = "AGOS_TOKEN"
wire_api = "responses"

[profiles.agos]
model_provider = "agos"
model = "programmer/php-dev"

[profiles.agos.auth]
# AGOS authenticates the profile token as a bearer token.
```

- `base_url` — the AGOS host plus the `/codex/v1` prefix; Codex appends
  `/responses` itself.
- `model` — `<proxy>/<route>`, exactly as on the other surfaces.
- `env_key` — Codex reads the token from this environment variable. Export
  your AGOS profile token: `export AGOS_TOKEN=<profile token>`.

Then run:

```sh
codex --profile agos
```

## 3. What the proxy does

| Codex sends | AGOS does |
|---|---|
| `input[]` message items | Preserved as ordered canonical messages, then translated to the selected provider's native input shape |
| `instructions` | Becomes the `system` message / Responses `instructions` |
| `tools[]` (function/custom) | Translated to native tool definitions for OpenAI, Responses, Anthropic, or Google |
| `function_call_output` items | Becomes a tool-result message and is translated back to the selected provider's native tool-result shape |
| `store`, `reasoning`, `include`, `prompt_cache_key`, … | Restored only for a Responses-compatible upstream; never sent to a strict chat provider |
| Upstream text delta | `response.output_text.delta` |
| Upstream tool call | `function_call` output item; `arguments` stays a JSON string |
| End of upstream stream | `response.output_item.done` items + `response.completed` (with usage) |

Input items that require server-side Responses state and cannot be represented
by a stateless destination are rejected explicitly; the proxy never drops them
quietly. Send the full replayable conversation in `input` (Codex does).

## 4. Limitations

- **Provider-native feature boundaries:** a request is sent only to a route
  entry that declares the required tools/media/JSON capability. A provider may
  still reject a particular payload; that attempt is logged and failover tries
  the next compatible entry.
- **Long turns:** the per-attempt timeout (`AGOS_ATTEMPT_TIMEOUT`, default
  120 s) and the request body cap (5 MB) apply; very long agentic sessions
  that exceed them will surface as upstream errors.

## 5. Verifying

```sh
curl -s http://127.0.0.1:3000/codex/v1/responses \
  -H "Authorization: Bearer $AGOS_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"model":"programmer/php-dev","input":"say hi","stream":true}' | tail -4
```

The last frame must be `response.completed`; Codex aborts the turn with
"stream closed before response.completed" otherwise.

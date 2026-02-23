# Getting Started with AGOS Proxy

> A step-by-step tutorial from installation to your first API request.

## Table of Contents

- [Install AGOS Proxy](#install-agos-proxy)
- [Create Your First Profile](#create-your-first-profile)
- [Add a Provider](#add-a-provider)
- [Create a Proxy and Route](#create-a-proxy-and-route)
- [Start the Server](#start-the-server)
- [Make Your First Request](#make-your-first-request)
- [Next Steps](#next-steps)

---

## Install AGOS Proxy

### Prerequisites

- **Rust** 1.75 or later. Install from [rustup.rs](https://rustup.rs/).
- A terminal.

### Build

```sh
git clone https://github.com/aivoo-app/agos-proxy.git
cd agos-proxy
cargo build --release
```

The binary is at `target/release/agos-proxy`. Optionally, add it to your PATH:

```sh
sudo cp target/release/agos-proxy /usr/local/bin/
```

Or install via cargo (places it in `~/.cargo/bin`):

```sh
cargo install --path .
```

### Verify

```sh
agos-proxy --version
# agos-proxy 0.1.0
```

---

## Create Your First Profile

A **profile** is a tenant — it owns providers, proxies, routes, and has its own API token.

```sh
agos-proxy profile create --name coder1
```

The CLI creates the profile and prints your **API token** (the profile `id`). Copy and save this — you'll use it to authenticate API requests.

```
Created profile "coder1" (id: abc123def456...).
API token: abc123def456...
```

> **Tip:** If you lose the token, you can rotate it with `agos-proxy profile token rotate coder1`. The old token stops working immediately.

### Where is data stored?

AGOS Proxy stores everything in a single SQLite file:

- Default: `~/.config/agos-proxy/agos.db`
- Override: set `AGOS_HOME=/path/to/dir`

---

## Add a Provider

A **provider** is an upstream LLM service. Let's add DeepSeek:

```sh
agos-proxy provider add --profile coder1
```

The interactive wizard prompts for:

| Prompt | Value | Notes |
|--------|-------|-------|
| Provider name | `deepseek` | Your label — choose anything |
| Base URL | `https://api.deepseek.com` | The provider's API endpoint |
| API token | `sk-...` | Your real key — stored encrypted |
| Provider kind | `OpenAI-compatible` | Default; covers most providers |

The provider token is encrypted with ChaCha20-Poly1305 before it reaches disk. You won't see it in plaintext again (only at creation/edit time).

### Add a Second Provider (for Failover)

```sh
agos-proxy provider add --profile coder1
# Provider name: anthropic
# Base URL: https://api.anthropic.com
# API token: sk-ant-...
# Provider kind: Anthropic
```

AGOS Proxy supports these provider kinds:

| Kind | Description |
|------|-------------|
| `OpenAI-compatible` | DeepSeek, OpenRouter, Together AI, Ollama, etc. |
| `Anthropic` | Native Claude API — request/response translation |
| `Google` | Native Gemini API — request/response translation |
| `Custom` | Reserved for future use |

---

## Create a Proxy and Route

### Create a Proxy

A **proxy** is a namespace for routes:

```sh
agos-proxy proxy create --profile coder1
# Proxy name: Programmer
```

### Create a Route

A **route** is a fallback chain of models. Callers request it as `<proxy>/<route>`:

```sh
agos-proxy route create --proxy Programmer
```

The wizard prompts for:

| Prompt | Value | Notes |
|--------|-------|-------|
| Route name | `php-dev` | Becomes `Programmer/php-dev` for callers |
| Routing strategy | `priority` | Try entries in order; fallback on failure |
| Model entries | (add at least one) | Pick provider + model string |

Add model entries to the chain:

```
Add a model entry:
  Provider: deepseek
  Model ID: deepseek-chat
  Priority: 1
  Weight: 1.0
  Supports tools? no
  Supports vision? no
  Supports JSON mode? no
  Add another? yes

Add a model entry:
  Provider: anthropic
  Model ID: claude-3-5-haiku-latest
  Priority: 2
  Weight: 1.0
  Supports tools? yes
  Supports vision? no
  Supports JSON mode? no
  Add another? no
```

Your chain now looks like:

```
Programmer/php-dev (priority strategy):
  1. deepseek/deepseek-chat         (priority 1 — tried first)
  2. anthropic/claude-3-5-haiku     (priority 2 — fallback)
```

---

## Start the Server

```sh
agos-proxy serve --bind 127.0.0.1:3000
```

You should see:

```
AGOS Proxy listening on 127.0.0.1:3000
```

### What happens on startup

1. The SQLite store is opened (migrations applied automatically)
2. The background health-probe loop starts (checks unhealthy providers every 30 seconds)
3. The HTTP server begins listening

### Run in the background

```sh
nohup agos-proxy serve --bind 127.0.0.1:3000 > agos.log 2>&1 &
```

Or use systemd — see [docs/deployment.md](docs/deployment.md).

---

## Make Your First Request

### With curl

```sh
curl http://localhost:3000/v1/chat/completions \
  -H "Authorization: Bearer YOUR_PROFILE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "Programmer/php-dev",
    "messages": [{"role": "user", "content": "Hello, world!"}]
  }'
```

Replace `YOUR_PROFILE_TOKEN` with the token printed at profile creation.

### With Python (OpenAI SDK)

```sh
pip install openai
```

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://localhost:3000/v1",
    api_key="YOUR_PROFILE_TOKEN",
)

response = client.chat.completions.create(
    model="Programmer/php-dev",
    messages=[{"role": "user", "content": "Hello, world!"}],
)
print(response.choices[0].message.content)
```

### With Node.js (OpenAI SDK)

```sh
npm install openai
```

```js
import OpenAI from 'openai';

const client = new OpenAI({
  baseURL: 'http://localhost:3000/v1',
  apiKey: 'YOUR_PROFILE_TOKEN',
});

const response = await client.chat.completions.create({
  model: 'Programmer/php-dev',
  messages: [{ role: 'user', content: 'Hello, world!' }],
});
console.log(response.choices[0].message.content);
```

### Streaming

Add `"stream": true` to the request body. The response is an SSE stream of OpenAI-compatible chunks.

```sh
curl http://localhost:3000/v1/chat/completions \
  -H "Authorization: Bearer YOUR_PROFILE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "Programmer/php-dev",
    "messages": [{"role": "user", "content": "Tell me a story"}],
    "stream": true
  }'
```

### List Available Models

```sh
curl http://localhost:3000/v1/models \
  -H "Authorization: Bearer YOUR_PROFILE_TOKEN"
```

Response:
```json
{
  "object": "list",
  "data": [
    {
      "id": "Programmer/php-dev",
      "object": "model",
      "owned_by": "agos"
    }
  ]
}
```

---

## Next Steps

- **Monitor usage:** `agos-proxy usage stats --profile coder1`
- **Check route health:** `agos-proxy route status --route php-dev`
- **Test interactively:** `agos-proxy chat` or `agos-proxy chat --tui`
- **Set rate limits:** `agos-proxy profile limit coder1 120`
- **Backup your config:** `agos-proxy config export --profile coder1`

### What to read next

- [docs/tutorials/common-patterns.md](common-patterns.md) — Multi-provider setups, per-team profiles, weighted routing
- [docs/tutorials/openai-sdk-integration.md](openai-sdk-integration.md) — Detailed OpenAI SDK integration for Python, JS, Go, and more
- [docs/providers.md](../providers.md) — Full provider setup reference
- [docs/failover.md](../failover.md) — How failover works and how to tune it

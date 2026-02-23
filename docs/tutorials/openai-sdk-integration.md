# OpenAI SDK Integration

> How to use AGOS Proxy with OpenAI SDKs in Python, JavaScript/TypeScript, Go, and other languages.

## Table of Contents

- [How It Works](#how-it-works)
- [Python](#python)
- [JavaScript / TypeScript](#javascript--typescript)
- [Go](#go)
- [Rust](#rust)
- [cURL](#curl)
- [Streaming](#streaming)
- [Error Handling](#error-handling)
- [Advanced Configuration](#advanced-configuration)
- [Troubleshooting SDK Issues](#troubleshooting-sdk-issues)

---

## How It Works

AGOS Proxy exposes an OpenAI-compatible API at `/v1/...`. Any OpenAI SDK client can be pointed at AGOS Proxy by changing two things:

1. **`base_url`** → your AGOS Proxy address (e.g., `http://localhost:3000/v1`)
2. **`api_key`** → your profile API token

Everything else stays the same. The model name you pass should be a route name in the format `<proxy>/<route>` (e.g., `Programmer/php-dev`).

---

## Python

### Install

```sh
pip install openai
```

### Basic Usage

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://localhost:3000/v1",
    api_key="YOUR_PROFILE_TOKEN",
)

response = client.chat.completions.create(
    model="Programmer/php-dev",
    messages=[
        {"role": "system", "content": "You are a helpful coding assistant."},
        {"role": "user", "content": "Write a Python function to sort a list."},
    ],
)

print(response.choices[0].message.content)
```

### Streaming

```python
stream = client.chat.completions.create(
    model="Programmer/php-dev",
    messages=[{"role": "user", "content": "Tell me a story."}],
    stream=True,
)

for chunk in stream:
    if chunk.choices and chunk.choices[0].delta.content:
        print(chunk.choices[0].delta.content, end="", flush=True)
```

### Tool / Function Calling

```python
tools = [
    {
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "Get the current weather in a location",
            "parameters": {
                "type": "object",
                "properties": {
                    "location": {"type": "string", "description": "The city name"},
                },
                "required": ["location"],
            },
        },
    }
]

response = client.chat.completions.create(
    model="Programmer/php-dev",
    messages=[{"role": "user", "content": "What's the weather in Tokyo?"}],
    tools=tools,
)
```

> **Note:** Tool/function calling only works if the underlying model supports it. Set the `capabilities.tools` flag on route entries so AGOS Proxy can skip models that don't support tools.

---

## JavaScript / TypeScript

### Install

```sh
npm install openai
```

### Basic Usage

```typescript
import OpenAI from 'openai';

const client = new OpenAI({
  baseURL: 'http://localhost:3000/v1',
  apiKey: 'YOUR_PROFILE_TOKEN',
});

async function main() {
  const response = await client.chat.completions.create({
    model: 'Programmer/php-dev',
    messages: [
      { role: 'system', content: 'You are a helpful coding assistant.' },
      { role: 'user', content: 'Write a function to sort an array.' },
    ],
  });

  console.log(response.choices[0].message.content);
}

main();
```

### Streaming

```typescript
const stream = await client.chat.completions.create({
  model: 'Programmer/php-dev',
  messages: [{ role: 'user', content: 'Tell me a story.' }],
  stream: true,
});

for await (const chunk of stream) {
  if (chunk.choices[0]?.delta?.content) {
    process.stdout.write(chunk.choices[0].delta.content);
  }
}
```

---

## Go

### Install

```sh
go get github.com/sashabaranov/go-openai
```

### Basic Usage

```go
package main

import (
    "context"
    "fmt"
    openai "github.com/sashabaranov/go-openai"
)

func main() {
    config := openai.DefaultConfig("YOUR_PROFILE_TOKEN")
    config.BaseURL = "http://localhost:3000/v1"
    client := openai.NewClientWithConfig(config)

    resp, err := client.CreateChatCompletion(
        context.Background(),
        openai.ChatCompletionRequest{
            Model: "Programmer/php-dev",
            Messages: []openai.ChatCompletionMessage{
                {Role: "system", Content: "You are a helpful coding assistant."},
                {Role: "user", Content: "Write a function to sort a slice."},
            },
        },
    )
    if err != nil {
        panic(err)
    }

    fmt.Println(resp.Choices[0].Message.Content)
}
```

---

## Rust

### Install

```sh
cargo add reqwest --features json
```

### Basic Usage

```rust
use reqwest::Client;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new();

    let response = client
        .post("http://localhost:3000/v1/chat/completions")
        .header("Authorization", "Bearer YOUR_PROFILE_TOKEN")
        .header("Content-Type", "application/json")
        .json(&json!({
            "model": "Programmer/php-dev",
            "messages": [
                {"role": "system", "content": "You are a helpful coding assistant."},
                {"role": "user", "content": "Write a Rust function to sort a vector."}
            ]
        }))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let content = response["choices"][0]["message"]["content"].as_str().unwrap();
    println!("{}", content);

    Ok(())
}
```

---

## cURL

### Non-Streaming

```sh
curl http://localhost:3000/v1/chat/completions \
  -H "Authorization: Bearer YOUR_PROFILE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "Programmer/php-dev",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'
```

### Streaming

```sh
curl -N http://localhost:3000/v1/chat/completions \
  -H "Authorization: Bearer YOUR_PROFILE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "Programmer/php-dev",
    "messages": [{"role": "user", "content": "Tell me a story."}],
    "stream": true
  }'
```

### List Models

```sh
curl http://localhost:3000/v1/models \
  -H "Authorization: Bearer YOUR_PROFILE_TOKEN"
```

### Text Completions

```sh
curl http://localhost:3000/v1/completions \
  -H "Authorization: Bearer YOUR_PROFILE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "Programmer/php-dev",
    "prompt": "Once upon a time",
    "max_tokens": 100
  }'
```

### Embeddings

```sh
curl http://localhost:3000/v1/embeddings \
  -H "Authorization: Bearer YOUR_PROFILE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "Programmer/php-dev",
    "input": "Hello, world!"
  }'
```

---

## Streaming

AGOS Proxy streams SSE responses that are compatible with the OpenAI streaming format:

```
data: {"id":"chatcmpl-xxx","object":"chat.completion.chunk","choices":[{"delta":{"role":"assistant"},"index":0,"finish_reason":null}]}

data: {"id":"chatcmpl-xxx","object":"chat.completion.chunk","choices":[{"delta":{"content":"Hello"},"index":0,"finish_reason":null}]}

data: [DONE]
```

### Known Limitation

If a streaming request fails **after** tokens have begun flowing (mid-stream), the stream breaks. This is a fundamental limitation of streaming failover — see [docs/failover.md](../failover.md) for details. Use non-streaming requests if you need guaranteed failover transparency.

---

## Error Handling

AGOS Proxy returns OpenAI-compatible error envelopes:

| HTTP | Error Type | Meaning |
|------|------------|---------|
| 400 | `invalid_request_error` | Malformed request body |
| 401 | `auth_error` | Missing or invalid bearer token |
| 429 | `rate_limit_error` | Rate limit exceeded (per-profile) |
| 404 | — | Model not found (no matching route) |
| 500 | — | Internal proxy error |
| 503 | — | No healthy provider in the chain |

Example error response:

```json
{
  "error": {
    "message": "rate limit exceeded: 120 requests per minute for this profile",
    "type": "rate_limit_error"
  }
}
```

Handle 429 with exponential backoff. The `Retry-After: 60` header tells you when the window resets.

---

## Advanced Configuration

### Custom Headers

If your proxy is behind a reverse proxy that adds headers, AGOS Proxy passes them through to the upstream provider. You can also configure per-provider extra headers:

```sh
agos-proxy provider edit --profile coder1
# Add headers like X-Custom: value when prompted
```

### Request Timeouts

The `serve` command uses a 10-second per-attempt timeout and 30-second HTTP timeout. These are not yet configurable via flags but can be adjusted in the source (`src/server/mod.rs`).

### Multiple Profiles

Each profile is isolated — separate rate limits, separate routes, separate usage logs. Use this to give different agents different budgets and routing rules.

---

## Troubleshooting SDK Issues

| Problem | Cause | Fix |
|---------|-------|-----|
| `401 Unauthorized` | Wrong or missing API token | Check `Authorization: Bearer YOUR_PROFILE_TOKEN` |
| `404 Not Found` | Model string doesn't match any route | List models: `GET /v1/models`. Use `<proxy>/<route>` format. |
| `429 Too Many Requests` | Rate limit hit | Add backoff. Increase limit: `agos-proxy profile limit <name> <rpm>` |
| `502 Bad Gateway` | All providers failed | Check `agos-proxy route status` and `agos-proxy usage recent` |
| Empty response | Model returned empty content | Check provider logs. The model may be rate-limited or misconfigured. |
| Slow first request | Cold start / health probe | Normal on first run. The health probe loop warms up over 30s. |
| Stream breaks mid-response | Provider failed during streaming | Use non-streaming for guaranteed failover, or add more fallback entries. |

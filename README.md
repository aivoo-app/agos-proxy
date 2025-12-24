# AGOS Proxy

A self-hosted, CLI-managed AI gateway that gives any agent an OpenAI-compatible
endpoint backed by automatic multi-provider failover.

AGOS Proxy sits between an agent and its LLM providers. Instead of an agent
calling `deepseek-v4-flash` directly, it calls a *route* such as
`programmer/php-developer-3.5-flash` that AGOS Proxy owns. Behind that single
route is an ordered list of real models from real providers; AGOS Proxy tries
them in priority order, tracks which are healthy, and transparently fails over
so a request keeps getting answered even if several underlying providers are
down at once.

Conceptually it is closest to LiteLLM's proxy/gateway mode, but with profile-based
multi-tenancy and failover as first-class concepts rather than add-ons.

## Highlights

- OpenAI-compatible `/v1/chat/completions`, `/v1/completions` and `/v1/models`
  endpoints — a drop-in replacement for any OpenAI SDK client.
- Multi-profile, multi-tenant setup with separate credentials and routing rules
  per agent, team, or use case.
- Ordered model fallback chains per route with automatic health tracking and
  background recovery.
- Streaming (SSE) passthrough that mirrors each provider's streaming behavior.
- CLI-first configuration with an interactive wizard and scriptable
  non-interactive flags.
- Per-profile secret storage, with optional password protection.
- Usage, cost, and latency visibility per model and per route.

## Quick start

The project is still in active development. Build the binary with:

```sh
cargo build --release
```

The CLI entry point is `agos-proxy`; `agos-proxy serve` starts the proxy server once the
routing engine and HTTP surface are in place. See `agos-proxy --help` for the current
command tree.

## Documentation

- Architecture and data model live under `docs/`.
- Build phases and the long-form design are tracked in the working plan.

## License

MIT
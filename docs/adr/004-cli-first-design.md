# ADR 004: CLI-First Design Philosophy

> Status: Accepted

## Context

AGOS Proxy needs a way for users to configure profiles, providers, proxies, and routes. The options are:

1. **Config file** (YAML, JSON, TOML) — edit a file, restart the server
2. **CLI commands** — run commands to manage the configuration
3. **HTTP API** — REST endpoints for management
4. **Web UI** — a browser-based dashboard

## Decision

We chose a **CLI-first** approach with interactive wizards and non-interactive flags. There is no config file and no management HTTP API or web UI.

## Consequences

### Positive

- **No config file to manage.** Configuration is stored in the SQLite store, not in files that can drift out of sync.
- **Interactive wizards.** First-time users are guided through setup with `agos-proxy setup` or per-command wizards.
- **Scriptable.** Every wizard has a flag-driven equivalent for CI and automation (`--profile`, `--name`, `--bind`, etc.).
- **Version-controllable.** Export a profile to JSON with `config export`, commit it, and reproduce the setup anywhere.
- **No management API surface.** Fewer endpoints to secure, fewer attack vectors.
- **No web UI to build and maintain.** Smaller codebase, less complexity.

### Negative

- **No hot-reload.** Changing configuration requires running a CLI command (but the changes take effect on the next request — no restart needed for most changes).
- **No web dashboard.** Users who prefer a browser UI must use the CLI.
- **Learning curve.** Users unfamiliar with CLI tools may need to learn the command structure.

### How Changes Take Effect

- **Provider changes** (new token, new base URL): Take effect immediately on the next request. The store is read on each request.
- **Route changes** (new entries, changed strategy): Take effect immediately on the next request.
- **Rate limit changes:** Take effect immediately.
- **Proxy/route CRUD:** Take effect immediately — no restart needed.

## The Setup Wizard

`agos-proxy setup` walks a first-time user through:
1. Create or pick a profile
2. Add providers
3. Create proxies
4. Build route chains
5. Show a summary

This is purely interactive — it never asks for automation.

## The Bootstrap Command

For scripted environments (CI, containers, provisioning), `agos-proxy bootstrap from-file` takes a JSON document describing a full profile tree and writes it to the store in one go.

```sh
agos-proxy bootstrap from-file setup.json
```

## The Config Export/Import Flow

For moving a setup between machines or backing up:

```sh
# Export (passphrase-sealed)
agos-proxy config export --profile coder1 --output backup.json

# Import (re-encrypts with destination master key)
agos-proxy config import --path backup.json
```

## Alternatives Considered

| Approach | Pros | Cons |
|----------|------|------|
| **YAML config file** | Human-readable, familiar | Drifts out of sync, no validation, needs hot-reload |
| **Management HTTP API** | Programmatic, no CLI needed | Security surface, more code to maintain |
| **Web UI** | Easy for non-CLI users | Significant development effort, security surface |
| **CLI (chosen)** | Scriptable, simple, no UI to maintain | Learning curve for non-CLI users |

The CLI-first approach was chosen because AGOS Proxy is a tool for developers and operators who are comfortable with the CLI. The interactive wizards lower the barrier for new users, and the flag-driven commands support automation.

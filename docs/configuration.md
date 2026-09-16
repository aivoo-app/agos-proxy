# AGOS Proxy — Configuration Reference

Where files live, environment variables, the bootstrap JSON schema, and how
secrets are handled.

## Data directory

AGOS Proxy keeps its SQLite store and metadata in a single directory. The
location is determined as follows:

1. If `AGOS_HOME` is set, use it directly.
2. Otherwise, use `$XDG_CONFIG_HOME/agos-proxy` (typically
   `~/.config/agos-proxy` on Linux).
3. On Windows, the equivalent appdata path is used.

You can override the location at any time with `AGOS_HOME`:

```sh
AGOS_HOME=/tmp/agos-test agos-proxy profile list
```

The directory is created automatically on first run.

### Contents

| Path                                          | Purpose                                            |
|-----------------------------------------------|----------------------------------------------------|
| `<data-dir>/agos.db`                          | SQLite store (profiles, providers, routes, usage). |
| `<data-dir>/master.key` (internal, meta table)| Master key for encrypting provider tokens.         |

There is no human-editable config file. Everything is administered through the
CLI, which writes to the store.

## Environment variables

| Variable       | Purpose                                                         | Default                              |
|----------------|-----------------------------------------------------------------|--------------------------------------|
| `AGOS_HOME`    | Override the data directory.                                    | `$XDG_CONFIG_HOME/agos-proxy`       |
| `RUST_LOG`     | Tracing filter for the `serve` command.                        | `info`                              |
| `AGOS_SETUP`   | (Docker / bootstrap only) JSON document to seed the store.     | —                                    |

`RUST_LOG` is honored by the `serve` command via `tracing-subscriber`. Common
values:

```sh
RUST_LOG=debug   agos-proxy serve
RUST_LOG=agos_proxy=debug   agos-proxy serve
RUST_LOG=off      agos-proxy serve
```

When set to `off`, the logging layer is entirely skipped at startup,
reducing idle memory and eliminating per-request formatting overhead.

## CLI configuration workflow

The CLI is the only way to configure AGOS Proxy. The typical sequence:

```sh
# 1. Create a profile (tenant + API token)
atos-proxy profile create --name coder1

# 2. Register a provider (upstream)
atos-proxy provider add --profile coder1

# 3. Create a proxy (route group)
atos-proxy proxy create --profile coder1

# 4. Create a route with a model chain
atos-proxy route create --proxy Programmer
```

Each command has an interactive wizard and a flag-driven variant. Use
`--help` on any subcommand for exact flags.

For first-time users, the guided wizard covers the whole sequence:

```sh
atos-proxy setup
```

## Bootstrap JSON schema

For scripted environments (CI, containers, provisioning), use
`atos-proxy bootstrap from-file`. The JSON document describes a full profile
tree.

### Top level

```json
{
  "profile": "coder1",
  "description": "optional free-text",
  "providers": [ ... ],
  "proxies": [ ... ]
}
```

| Field         | Required | Type   | Notes                              |
|---------------|----------|--------|------------------------------------|
| `profile`     | yes      | string | Profile name; becomes the tenant.  |
| `description` | no       | string | Free-text description.             |
| `providers`   | no       | array  | Upstream providers to register.    |
| `proxies`     | no       | array  | Proxies with their routes.         |

### Provider

```json
{
  "name": "deepseek",
  "description": "optional",
  "base_url": "https://api.deepseek.com",
  "auth_token": "sk-...",
  "kind": "openai",
  "extra_headers": { "X-Custom": "value" }
}
```

| Field          | Required | Type      | Notes                                                    |
|----------------|----------|-----------|----------------------------------------------------------|
| `name`         | yes      | string    | Provider name.                                           |
| `description`  | no       | string    | Free-text.                                               |
| `base_url`     | yes      | string    | Upstream base URL.                                       |
| `auth_token`   | yes      | string    | Upstream API token; stored encrypted at rest.           |
| `kind`         | no       | string    | One of `openai`, `openai_responses`, `anthropic`, `google`, `custom`. |
| `extra_headers`| no       | object    | Extra headers sent with every upstream request.         |

Defaults: `kind` = `openai`; `extra_headers` = `{}`.

### Proxy

```json
{
  "name": "Programmer",
  "description": "optional",
  "routes": [ ... ]
}
```

| Field          | Required | Type   | Notes                              |
|----------------|----------|--------|------------------------------------|
| `name`         | yes      | string | Proxy name.                        |
| `description`  | no       | string | Free-text.                         |
| `routes`       | no       | array  | Routes under this proxy.          |

### Route

```json
{
  "name": "php-developer-3.5-flash",
  "description": "optional",
  "strategy": "priority",
  "models": [ ... ]
}
```

| Field          | Required | Type   | Notes                                                    |
|----------------|----------|--------|----------------------------------------------------------|
| `name`         | yes      | string | Route name.                                              |
| `description`  | no       | string | Free-text.                                               |
| `strategy`     | no       | string | One of `priority`, `round_robin`, `weighted`.           |
| `models`       | no       | array  | Models in the chain, in the declared order.             |

Defaults: `strategy` = `priority`.

### Route model entry

```json
{
  "provider": "deepseek",
  "model": "deepseek-v4-flash",
  "priority": 1,
  "weight": 1.0,
  "capabilities": {
    "tools": true,
    "vision": false,
    "json_mode": true,
    "max_context": 128000
  }
}
```

| Field          | Required | Type   | Notes                                                    |
|----------------|----------|--------|----------------------------------------------------------|
| `provider`     | yes      | string | Provider `name` (must already be declared in `providers`).|
| `model`        | yes      | string | Model string the provider expects.                       |
| `priority`     | no       | number | Position in the chain; defaults to declaration order.   |
| `weight`       | no       | number | Relative weight; defaults to `1.0`.                     |
| `capabilities` | no       | object | Feature flags. Defaults to all-true when omitted.       |

`capabilities` fields default to `true` when omitted (the conservative choice
for bootstrap — you can always restrict later with `route model` commands).

### Example

```json
{
  "profile": "docker-tenant",
  "description": "created by docker compose",
  "providers": [
    {
      "name": "mock",
      "base_url": "http://mock:9999",
      "auth_token": "sk-mock",
      "kind": "openai"
    }
  ],
  "proxies": [
    {
      "name": "main",
      "routes": [
        {
          "name": "chat",
          "models": [
            { "provider": "mock", "model": "mock-model" }
          ]
        }
      ]
    }
  ]
}
```

Run with:

```sh
atos-proxy bootstrap from-file setup.json
# prints the generated API token to stdout
```

Or from stdin:

```sh
cat setup.json | agos-proxy bootstrap from-file -
```

## Secrets

### Provider tokens

Provider `auth_token` values are encrypted before they reach disk. Encryption
uses ChaCha20-Poly1305 with a 256-bit master key. The master key is randomly
generated on first store creation and stored in the `meta` table (itself not
exposed through any CLI command).

Consequences:

- You cannot read provider tokens back through the CLI — they are shown only
  at creation/editing time, and never in `list` output.
- Export/import re-encrypts tokens with the destination store's master key.
- If the store file is lost, provider tokens are lost. Back up the data
  directory if you need to persist them.

### Profile passwords

A profile may be password-protected to gate interactive management operations
(create, edit, delete, provider/route changes). The password is hashed with
Argon2id using a random 16-byte salt. The stored form is:

```
argon2id:<salt-hex>:<derived-key-hex>
```

Verification re-derives the key from the recorded salt and compares in constant
time.

Profiles without a password are open for management from any local CLI session.
Profiles with a password prompt on every mutating operation (up to 3 attempts).

### Password migration

Older stores may contain `sha256:`-prefixed hashes (a marker format used before
the Argon2id milestone). These are still verified for backward compatibility,
but new passwords are always hashed with Argon2id.

## Portable config files

`atos-proxy config export` writes a passphrase-sealed file containing the full
profile tree (providers with their upstream tokens, proxies, routes, entries).
`atos-proxy config import` restores it into a store, re-encrypting tokens with
the local master key and issuing a fresh bearer token.

This is the intended mechanism for moving a setup between machines or backing
it up — never copy the raw SQLite file between stores with different master
keys.

See `atos-proxy config --help` for exact flags.

# Command-Line Help for `agos-proxy`

This document contains the help content for the `agos-proxy` command-line program.

**Command Overview:**

* [`agos-proxy`↴](#agos-proxy)
* [`agos-proxy serve`↴](#agos-proxy-serve)
* [`agos-proxy profile`↴](#agos-proxy-profile)
* [`agos-proxy profile create`↴](#agos-proxy-profile-create)
* [`agos-proxy profile list`↴](#agos-proxy-profile-list)
* [`agos-proxy profile show`↴](#agos-proxy-profile-show)
* [`agos-proxy profile token`↴](#agos-proxy-profile-token)
* [`agos-proxy profile token rotate`↴](#agos-proxy-profile-token-rotate)
* [`agos-proxy profile limit`↴](#agos-proxy-profile-limit)
* [`agos-proxy profile edit`↴](#agos-proxy-profile-edit)
* [`agos-proxy profile delete`↴](#agos-proxy-profile-delete)
* [`agos-proxy provider`↴](#agos-proxy-provider)
* [`agos-proxy provider add`↴](#agos-proxy-provider-add)
* [`agos-proxy provider list`↴](#agos-proxy-provider-list)
* [`agos-proxy provider edit`↴](#agos-proxy-provider-edit)
* [`agos-proxy provider delete`↴](#agos-proxy-provider-delete)
* [`agos-proxy proxy`↴](#agos-proxy-proxy)
* [`agos-proxy proxy create`↴](#agos-proxy-proxy-create)
* [`agos-proxy proxy list`↴](#agos-proxy-proxy-list)
* [`agos-proxy proxy edit`↴](#agos-proxy-proxy-edit)
* [`agos-proxy proxy delete`↴](#agos-proxy-proxy-delete)
* [`agos-proxy route`↴](#agos-proxy-route)
* [`agos-proxy route create`↴](#agos-proxy-route-create)
* [`agos-proxy route status`↴](#agos-proxy-route-status)
* [`agos-proxy route edit`↴](#agos-proxy-route-edit)
* [`agos-proxy route delete`↴](#agos-proxy-route-delete)
* [`agos-proxy route economy`↴](#agos-proxy-route-economy)
* [`agos-proxy route model`↴](#agos-proxy-route-model)
* [`agos-proxy route model add`↴](#agos-proxy-route-model-add)
* [`agos-proxy route model remove`↴](#agos-proxy-route-model-remove)
* [`agos-proxy route model move`↴](#agos-proxy-route-model-move)
* [`agos-proxy route model price`↴](#agos-proxy-route-model-price)
* [`agos-proxy chat`↴](#agos-proxy-chat)
* [`agos-proxy config`↴](#agos-proxy-config)
* [`agos-proxy config export`↴](#agos-proxy-config-export)
* [`agos-proxy config import`↴](#agos-proxy-config-import)
* [`agos-proxy usage`↴](#agos-proxy-usage)
* [`agos-proxy usage stats`↴](#agos-proxy-usage-stats)
* [`agos-proxy usage recent`↴](#agos-proxy-usage-recent)
* [`agos-proxy bootstrap`↴](#agos-proxy-bootstrap)
* [`agos-proxy bootstrap from-file`↴](#agos-proxy-bootstrap-from-file)
* [`agos-proxy setup`↴](#agos-proxy-setup)
* [`agos-proxy gen-docs`↴](#agos-proxy-gen-docs)

## `agos-proxy`

Self-hosted multi-provider AI gateway with failover

**Usage:** `agos-proxy <COMMAND>`

###### **Subcommands:**

* `serve` — Start the proxy server
* `profile` — Manage profiles (tenants) and their API tokens
* `provider` — Manage providers (upstreams + credentials) for a profile
* `proxy` — Manage proxies for a profile
* `route` — Manage routes and their ordered model chains
* `chat` — Test a proxy/route interactively before wiring up a client
* `config` — Export or import a profile setup
* `usage` — Show per-request usage logs and aggregates for a profile
* `bootstrap` — Seed a profile setup non-interactively from a JSON document
* `setup` — Guided terminal setup wizard: profile -> providers -> proxies -> routes
* `gen-docs` — Generate developer documentation (CLI reference, man pages, completions)



## `agos-proxy serve`

Start the proxy server

**Usage:** `agos-proxy serve [OPTIONS]`

###### **Options:**

* `--bind <BIND>` — Address to bind, e.g. 0.0.0.0:8080

  Default value: `127.0.0.1:3000`
* `--attempt-timeout <ATTEMPT_TIMEOUT>` — Per-attempt timeout in seconds: how long one model may take before the router fails over to the next entry in the chain
* `--stream-idle-timeout <STREAM_IDLE_TIMEOUT>` — Idle timeout in seconds for committed streams: how long the upstream may stay silent between body chunks before the stream is failed instead of hanging the client



## `agos-proxy profile`

Manage profiles (tenants) and their API tokens

**Usage:** `agos-proxy profile <COMMAND>`

###### **Subcommands:**

* `create` — Create a new profile (interactive wizard, or flag-driven)
* `list` — List existing profiles and their API token identifiers
* `show` — Show a single profile in full
* `token` — Manage a profile's API token
* `limit` — View or change a profile's requests-per-minute limit (0 = unlimited)
* `edit` — Edit a profile's name, description and password
* `delete` — Delete a profile and everything under it



## `agos-proxy profile create`

Create a new profile (interactive wizard, or flag-driven)

**Usage:** `agos-proxy profile create [OPTIONS]`

###### **Options:**

* `--name <NAME>` — Name of the profile, e.g. `coder1`



## `agos-proxy profile list`

List existing profiles and their API token identifiers

**Usage:** `agos-proxy profile list`



## `agos-proxy profile show`

Show a single profile in full

**Usage:** `agos-proxy profile show <NAME>`

###### **Arguments:**

* `<NAME>` — Name of the profile



## `agos-proxy profile token`

Manage a profile's API token

**Usage:** `agos-proxy profile token <COMMAND>`

###### **Subcommands:**

* `rotate` — Generate a new API token for the profile



## `agos-proxy profile token rotate`

Generate a new API token for the profile

**Usage:** `agos-proxy profile token rotate <NAME>`

###### **Arguments:**

* `<NAME>` — Name of the profile



## `agos-proxy profile limit`

View or change a profile's requests-per-minute limit (0 = unlimited)

**Usage:** `agos-proxy profile limit <NAME> [RPM]`

###### **Arguments:**

* `<NAME>` — Name of the profile
* `<RPM>` — New requests-per-minute ceiling; omit to just show the current one



## `agos-proxy profile edit`

Edit a profile's name, description and password

**Usage:** `agos-proxy profile edit [OPTIONS]`

###### **Options:**

* `--name <NAME>` — Name of the profile to edit



## `agos-proxy profile delete`

Delete a profile and everything under it

**Usage:** `agos-proxy profile delete [OPTIONS]`

###### **Options:**

* `--name <NAME>` — Name of the profile to delete
* `--yes` — Skip the confirmation prompt (for scripts/CI)



## `agos-proxy provider`

Manage providers (upstreams + credentials) for a profile

**Usage:** `agos-proxy provider <COMMAND>`

###### **Subcommands:**

* `add` — Add a provider to a profile (interactive wizard, or flag-driven)
* `list` — List the providers configured on a profile
* `edit` — Edit a provider's settings interactively
* `delete` — Remove a provider



## `agos-proxy provider add`

Add a provider to a profile (interactive wizard, or flag-driven)

**Usage:** `agos-proxy provider add [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile
* `--name <NAME>` — Provider name (e.g. `upstream`); triggers non-interactive mode when combined with `--base-url` and `--auth-token`
* `--base-url <BASE_URL>` — Upstream base URL
* `--auth-token <AUTH_TOKEN>` — Upstream API token
* `--kind <KIND>` — Provider kind (openai | openai_responses | anthropic | google | custom)
* `--description <DESCRIPTION>` — Free-text description
* `--header <HEADERS>` — Extra header sent upstream, as `Name: value` (repeatable)



## `agos-proxy provider list`

List the providers configured on a profile

**Usage:** `agos-proxy provider list [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile



## `agos-proxy provider edit`

Edit a provider's settings interactively

**Usage:** `agos-proxy provider edit [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile



## `agos-proxy provider delete`

Remove a provider

**Usage:** `agos-proxy provider delete [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile
* `--provider <PROVIDER>` — Name of the provider to remove
* `--yes` — Skip the confirmation prompt (for scripts/CI)



## `agos-proxy proxy`

Manage proxies for a profile

**Usage:** `agos-proxy proxy <COMMAND>`

###### **Subcommands:**

* `create` — Create a new proxy under a profile (interactive wizard, or flag-driven)
* `list` — List the proxies under a profile
* `edit` — Edit a proxy's name/description
* `delete` — Remove a proxy (and its routes)



## `agos-proxy proxy create`

Create a new proxy under a profile (interactive wizard, or flag-driven)

**Usage:** `agos-proxy proxy create [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile



## `agos-proxy proxy list`

List the proxies under a profile

**Usage:** `agos-proxy proxy list [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile



## `agos-proxy proxy edit`

Edit a proxy's name/description

**Usage:** `agos-proxy proxy edit [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile



## `agos-proxy proxy delete`

Remove a proxy (and its routes)

**Usage:** `agos-proxy proxy delete [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile



## `agos-proxy route`

Manage routes and their ordered model chains

**Usage:** `agos-proxy route <COMMAND>`

###### **Subcommands:**

* `create` — Create a new route under a proxy (interactive wizard, or flag-driven)
* `status` — Show the live health state of every model in a route's chain
* `edit` — Edit a route's name, description and routing strategy
* `delete` — Remove a route and its model chain
* `economy` — Tune economy limits: max_tokens clamp + exact-cache TTL
* `model` — Manage the models in a route's fallback chain



## `agos-proxy route create`

Create a new route under a proxy (interactive wizard, or flag-driven)

**Usage:** `agos-proxy route create [OPTIONS]`

###### **Options:**

* `--proxy <PROXY>` — Name of the owning proxy



## `agos-proxy route status`

Show the live health state of every model in a route's chain

**Usage:** `agos-proxy route status [OPTIONS]`

###### **Options:**

* `--route <ROUTE>` — Name of the route



## `agos-proxy route edit`

Edit a route's name, description and routing strategy

**Usage:** `agos-proxy route edit [OPTIONS]`

###### **Options:**

* `--proxy <PROXY>` — Name of the owning proxy



## `agos-proxy route delete`

Remove a route and its model chain

**Usage:** `agos-proxy route delete [OPTIONS]`

###### **Options:**

* `--proxy <PROXY>` — Name of the owning proxy



## `agos-proxy route economy`

Tune economy limits: max_tokens clamp + exact-cache TTL

**Usage:** `agos-proxy route economy [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile
* `--proxy <PROXY>` — Name of the owning proxy
* `--route <ROUTE>` — Name of the route (skips the route picker)
* `--max-tokens <MAX_TOKENS>` — Max tokens ceiling (0 = passthrough)
* `--cache-ttl <CACHE_TTL>` — Exact-cache TTL seconds (0 = disabled)



## `agos-proxy route model`

Manage the models in a route's fallback chain

**Usage:** `agos-proxy route model <COMMAND>`

###### **Subcommands:**

* `add` — Add a model to a route's chain
* `remove` — Remove a model from a route's chain
* `move` — Move a model to a new position in the chain
* `price` — Set blended price ($/1M tokens) used by Economy sorting



## `agos-proxy route model add`

Add a model to a route's chain

**Usage:** `agos-proxy route model add [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile
* `--proxy <PROXY>` — Name of the owning proxy
* `--route <ROUTE>` — Name of the route (skips the route picker)
* `--provider <PROVIDER>` — Provider name (skips the provider picker)
* `--model <MODEL_ID>` — Model ID (e.g. `openai/provider-model`; skips the model prompt)
* `--weight <WEIGHT>` — Weighted-strategy share; defaults to 1.0
* `--price <PRICE>` — Blended price USD/1M tokens for Economy sorting
* `--yes` — Skip the capability prompts (defaults everything on)



## `agos-proxy route model remove`

Remove a model from a route's chain

**Usage:** `agos-proxy route model remove [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile
* `--proxy <PROXY>` — Name of the owning proxy
* `--route <ROUTE>` — Name of the route (skips the route picker)
* `--model <MODEL_ID>` — Model ID to remove (skips the model picker)
* `--yes` — Skip the confirmation prompt (for scripts/CI)



## `agos-proxy route model move`

Move a model to a new position in the chain

**Usage:** `agos-proxy route model move [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile
* `--proxy <PROXY>` — Name of the owning proxy
* `--route <ROUTE>` — Name of the route (skips the route picker)
* `--model <MODEL_ID>` — Model ID to move (skips the model picker)
* `--position <POSITION>` — New 1-based priority position



## `agos-proxy route model price`

Set blended price ($/1M tokens) used by Economy sorting

**Usage:** `agos-proxy route model price [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile
* `--proxy <PROXY>` — Name of the owning proxy
* `--route <ROUTE>` — Name of the route (skips the route picker)
* `--model <MODEL_ID>` — Model ID to price (skips the model picker)
* `--price <PRICE>` — Blended price in USD per 1M tokens



## `agos-proxy chat`

Test a proxy/route interactively before wiring up a client

**Usage:** `agos-proxy chat [OPTIONS]`

###### **Options:**

* `--tui` — Run the conversation in a full-screen terminal UI instead of the line-by-line REPL



## `agos-proxy config`

Export or import a profile setup

**Usage:** `agos-proxy config <COMMAND>`

###### **Subcommands:**

* `export` — Export a profile's setup (providers, proxies, routes) to a portable file
* `import` — Import a profile setup previously written by `export`



## `agos-proxy config export`

Export a profile's setup (providers, proxies, routes) to a portable file

**Usage:** `agos-proxy config export [OPTIONS] [PROFILE]`

###### **Arguments:**

* `<PROFILE>` — Name of the profile to export

###### **Options:**

* `--output <OUTPUT>` — Output file; defaults to `<profile>.agos.json` in the current dir



## `agos-proxy config import`

Import a profile setup previously written by `export`

**Usage:** `agos-proxy config import [OPTIONS] [PATH]`

###### **Arguments:**

* `<PATH>` — Path to the portable profile file

###### **Options:**

* `--name <NAME>` — Name for the imported profile if the original name is taken



## `agos-proxy usage`

Show per-request usage logs and aggregates for a profile

**Usage:** `agos-proxy usage <COMMAND>`

###### **Subcommands:**

* `stats` — Aggregate calls, failures, latency and tokens per model
* `recent` — The most recent individual requests, newest first



## `agos-proxy usage stats`

Aggregate calls, failures, latency and tokens per model

**Usage:** `agos-proxy usage stats [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the profile to report on



## `agos-proxy usage recent`

The most recent individual requests, newest first

**Usage:** `agos-proxy usage recent [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the profile to report on
* `--limit <LIMIT>` — How many records to show

  Default value: `20`



## `agos-proxy bootstrap`

Seed a profile setup non-interactively from a JSON document

**Usage:** `agos-proxy bootstrap <COMMAND>`

###### **Subcommands:**

* `from-file` — Seed the store from a JSON setup file and print the API token



## `agos-proxy bootstrap from-file`

Seed the store from a JSON setup file and print the API token

**Usage:** `agos-proxy bootstrap from-file <PATH>`

###### **Arguments:**

* `<PATH>` — Path to the JSON setup document, or `-` for stdin



## `agos-proxy setup`

Guided terminal setup wizard: profile -> providers -> proxies -> routes

**Usage:** `agos-proxy setup`



## `agos-proxy gen-docs`

Generate developer documentation (CLI reference, man pages, completions)

**Usage:** `agos-proxy gen-docs [OPTIONS]`

###### **Options:**

* `--output-dir <DIR>` — Directory to write the generated docs into (created if missing)

  Default value: `./target/docs`
* `--all` — Also generate the roff man page and shell completions



<hr/>

<small><i>
    This document was generated automatically by
    <a href="https://crates.io/crates/clap-markdown"><code>clap-markdown</code></a>.
</i></small>

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
* [`agos-proxy provider share`↴](#agos-proxy-provider-share)
* [`agos-proxy mask`↴](#agos-proxy-mask)
* [`agos-proxy mask add`↴](#agos-proxy-mask-add)
* [`agos-proxy mask list`↴](#agos-proxy-mask-list)
* [`agos-proxy mask set`↴](#agos-proxy-mask-set)
* [`agos-proxy mask set-default`↴](#agos-proxy-mask-set-default)
* [`agos-proxy mask delete`↴](#agos-proxy-mask-delete)
* [`agos-proxy mask test`↴](#agos-proxy-mask-test)
* [`agos-proxy mask audit`↴](#agos-proxy-mask-audit)
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
* [`agos-proxy route share`↴](#agos-proxy-route-share)
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
* `mask` — Manage egress masks (network identities per provider)
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
* `limit` — View or change a profile's requests-per-minute limit (0 = unlimited). Note: rate limiting is per-process; multiple proxy instances each enforce their own limit independently
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

View or change a profile's requests-per-minute limit (0 = unlimited). Note: rate limiting is per-process; multiple proxy instances each enforce their own limit independently

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
* `share` — Publish (or unpublish) a provider across every profile on this instance



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
* `--mask <MASK>` — Egress mask to bind this key to (see `agos-proxy mask --help`)
* `--share <SHARE>` — Publish this provider to every profile on this instance. Only the owning profile can edit it; other profiles may route through it

  Possible values: `true`, `false`




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



## `agos-proxy provider share`

Publish (or unpublish) a provider across every profile on this instance

**Usage:** `agos-proxy provider share [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile
* `--provider <PROVIDER>` — Name of the provider to publish
* `--share <SHARE>` — `--share true` publishes; `--share false` unpublishes. Omit to ask

  Possible values: `true`, `false`




## `agos-proxy mask`

Manage egress masks (network identities per provider)

**Usage:** `agos-proxy mask <COMMAND>`

###### **Subcommands:**

* `add` — Register a new egress hop (interactive wizard, or flag-driven)
* `list` — List the masks configured on a profile (secrets are never shown)
* `set` — Change a mask's settings
* `set-default` — Make a mask the profile-wide fallback for providers without their own
* `delete` — Remove a mask. Providers bound to it fall back to the profile default
* `test` — Verify a mask end to end: secret, probe reply and (optionally) that its egress identity matches expectations. `--repeat` hammers the probe to measure identity stability (a VPS should be constant; a serverless platform rotates within its pool)
* `audit` — Probe every mask on a profile and report the egress identities, warning when two masks share an ASN (five deployments on one platform are one upstream-visible identity). Exits non-zero on duplicate ASNs



## `agos-proxy mask add`

Register a new egress hop (interactive wizard, or flag-driven)

**Usage:** `agos-proxy mask add [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile
* `--name <NAME>` — Mask name (e.g. `opencode-key-1`); triggers non-interactive mode when combined with `--endpoint-url` and `--secret`
* `--kind <KIND>` — Backend label: cf_worker | lambda | cloud_run | nginx | vps | commercial
* `--endpoint-url <ENDPOINT_URL>` — Absolute URL of the hop
* `--secret <SECRET>` — Shared secret the hop expects in `X-forward-mask`
* `--max-body-bytes <MAX_BODY_BYTES>` — Skip this hop for request bodies larger than this many bytes (0 = no limit)

  Default value: `0`
* `--expected-egress-ip <EXPECTED_EGRESS_IP>` — Egress IP the probe must report, when it is stable (e.g. a VPS)



## `agos-proxy mask list`

List the masks configured on a profile (secrets are never shown)

**Usage:** `agos-proxy mask list [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile



## `agos-proxy mask set`

Change a mask's settings

**Usage:** `agos-proxy mask set [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile
* `--mask <MASK>` — Mask to change
* `--kind <KIND>`
* `--endpoint-url <ENDPOINT_URL>`
* `--secret <SECRET>` — New shared secret (empty to keep the current one)
* `--max-body-bytes <MAX_BODY_BYTES>`
* `--expected-egress-ip <EXPECTED_EGRESS_IP>` — Expected egress IP; `none` clears the expectation
* `--share <SHARE>` — Publish (or unpublish) this mask across every profile

  Possible values: `true`, `false`




## `agos-proxy mask set-default`

Make a mask the profile-wide fallback for providers without their own

**Usage:** `agos-proxy mask set-default [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile
* `--mask <MASK>` — Mask to use as the default, or `none` to clear



## `agos-proxy mask delete`

Remove a mask. Providers bound to it fall back to the profile default

**Usage:** `agos-proxy mask delete [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile
* `--mask <MASK>` — Name of the mask to remove
* `--yes` — Skip the confirmation prompt (for scripts/CI)



## `agos-proxy mask test`

Verify a mask end to end: secret, probe reply and (optionally) that its egress identity matches expectations. `--repeat` hammers the probe to measure identity stability (a VPS should be constant; a serverless platform rotates within its pool)

**Usage:** `agos-proxy mask test [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile
* `--mask <MASK>` — Mask to test
* `--egress-ip` — Also verify the reported egress IP against `expected_egress_ip`
* `--repeat <REPEAT>` — How many probes to send

  Default value: `1`



## `agos-proxy mask audit`

Probe every mask on a profile and report the egress identities, warning when two masks share an ASN (five deployments on one platform are one upstream-visible identity). Exits non-zero on duplicate ASNs

**Usage:** `agos-proxy mask audit [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile



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
* `share` — Publish (or unpublish) a route so other profiles can add it to their own chains (route-as-model)
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



## `agos-proxy route share`

Publish (or unpublish) a route so other profiles can add it to their own chains (route-as-model)

**Usage:** `agos-proxy route share [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile
* `--proxy <PROXY>` — Name of the owning proxy
* `--route <ROUTE>` — Name of the route (skips the route picker)
* `--share <SHARE>` — `--share true` publishes; `--share false` unpublishes. Omit to ask

  Possible values: `true`, `false`




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
* `--yes` — Skip the capability prompts (defaults to no capabilities; use `route model edit` later if the model supports tools/vision/JSON)
* `--from-route <FROM_ROUTE>` — Add another route as a model instead of a provider model. Accepts `<proxy>/<route>` (same profile) or `<profile>/<proxy>/<route>` for a shared foreign route. Only another profile's *shared* routes work



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
* `--by-key` — Aggregate per upstream key (provider) instead of per model, showing each key's egress mask and how often it was rate limited



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

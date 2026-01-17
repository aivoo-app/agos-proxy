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
* [`agos-proxy provider`↴](#agos-proxy-provider)
* [`agos-proxy provider add`↴](#agos-proxy-provider-add)
* [`agos-proxy provider list`↴](#agos-proxy-provider-list)
* [`agos-proxy proxy`↴](#agos-proxy-proxy)
* [`agos-proxy proxy create`↴](#agos-proxy-proxy-create)
* [`agos-proxy route`↴](#agos-proxy-route)
* [`agos-proxy route create`↴](#agos-proxy-route-create)
* [`agos-proxy route status`↴](#agos-proxy-route-status)
* [`agos-proxy chat`↴](#agos-proxy-chat)
* [`agos-proxy config`↴](#agos-proxy-config)
* [`agos-proxy config export`↴](#agos-proxy-config-export)
* [`agos-proxy config import`↴](#agos-proxy-config-import)
* [`agos-proxy usage`↴](#agos-proxy-usage)
* [`agos-proxy usage stats`↴](#agos-proxy-usage-stats)
* [`agos-proxy usage recent`↴](#agos-proxy-usage-recent)
* [`agos-proxy bootstrap`↴](#agos-proxy-bootstrap)
* [`agos-proxy bootstrap from-file`↴](#agos-proxy-bootstrap-from-file)

## `agos-proxy`

Self-hosted OpenAI-compatible AI gateway with multi-provider failover

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



## `agos-proxy serve`

Start the proxy server

**Usage:** `agos-proxy serve [OPTIONS]`

###### **Options:**

* `--bind <BIND>` — Address to bind, e.g. 0.0.0.0:8080

  Default value: `127.0.0.1:3000`



## `agos-proxy profile`

Manage profiles (tenants) and their API tokens

**Usage:** `agos-proxy profile <COMMAND>`

###### **Subcommands:**

* `create` — Create a new profile (interactive wizard, or flag-driven)
* `list` — List existing profiles and their API token identifiers
* `show` — Show a single profile in full
* `token` — Manage a profile's API token
* `limit` — View or change a profile's requests-per-minute limit (0 = unlimited)



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



## `agos-proxy provider`

Manage providers (upstreams + credentials) for a profile

**Usage:** `agos-proxy provider <COMMAND>`

###### **Subcommands:**

* `add` — Add a provider to a profile (interactive wizard, or flag-driven)
* `list` — List the providers configured on a profile



## `agos-proxy provider add`

Add a provider to a profile (interactive wizard, or flag-driven)

**Usage:** `agos-proxy provider add [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile



## `agos-proxy provider list`

List the providers configured on a profile

**Usage:** `agos-proxy provider list [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile



## `agos-proxy proxy`

Manage proxies for a profile

**Usage:** `agos-proxy proxy <COMMAND>`

###### **Subcommands:**

* `create` — Create a new proxy under a profile (interactive wizard, or flag-driven)



## `agos-proxy proxy create`

Create a new proxy under a profile (interactive wizard, or flag-driven)

**Usage:** `agos-proxy proxy create [OPTIONS]`

###### **Options:**

* `--profile <PROFILE>` — Name of the owning profile



## `agos-proxy route`

Manage routes and their ordered model chains

**Usage:** `agos-proxy route <COMMAND>`

###### **Subcommands:**

* `create` — Create a new route under a proxy (interactive wizard, or flag-driven)
* `status` — Show the live health state of every model in a route's chain



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



## `agos-proxy chat`

Test a proxy/route interactively before wiring up a client

**Usage:** `agos-proxy chat`



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



<hr/>

<small><i>
    This document was generated automatically by
    <a href="https://crates.io/crates/clap-markdown"><code>clap-markdown</code></a>.
</i></small>

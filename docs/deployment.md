# Deployment

How to run AGOS Proxy in different environments: Docker, bare binary, and
systemd.

## Binary installation

Build the release binary:

```sh
cargo build --release
```

The binary is `target/release/agos-proxy`. Move it somewhere on your `PATH`:

```sh
sudo cp target/release/agos-proxy /usr/local/bin/
```

Verify:

```sh
atos-proxy --version
```

Then create a profile and start the server:

```sh
atos-proxy profile create --name myagent
atos-proxy serve --bind 0.0.0.0:8080
```

To install via `cargo install` (if you have the source tree and cargo):

```sh
cargo install --path .
```

This places `atos-proxy` in `~/.cargo/bin`.

## Docker

The repository includes a production-oriented `Dockerfile` and a
`docker-compose.yml` for the test stack.

### Using the Docker image

Build:

```sh
docker build -t agos-proxy .
```

Run:

```sh
mkdir -p /data/agos
docker run -d --name agos-proxy \
  -p 8080:8080 \
  -v /data/agos:/data \
  -e AGOS_HOME=/data \
  agos-proxy serve --bind 0.0.0.0:8080
```

The entrypoint seeds the store on first run if `AGOS_SETUP` is set and the
store is empty; otherwise it just starts the server.

### Docker Compose test stack

The compose file defines two services: a mock upstream and the proxy. Start it:

```sh
docker compose up -d
```

Stop and wipe state:

```sh
docker compose down -v
```

This is the easiest way to try AGOS Proxy without installing anything locally.
See `Makefile` targets `docker-up` and `docker-down`.

### Customizing the Docker image

The `Dockerfile` is a two-stage build:

1. **Builder** — `rust:1-slim`; compiles the release binary.
2. **Runtime** — `debian:trixie-slim`; copies the binary, adds `ca-certificates`
   and the entrypoint script, sets `AGOS_HOME=/data`, and exposes port 8080.

To bake in a default setup, override the entrypoint or pass `AGOS_SETUP`. To
change the bind address, change the `CMD` in the Dockerfile or pass it on the
`docker run` command line.

## systemd service

For a long-running instance on a Linux host, wrap the binary in a systemd unit.

Example unit file at `/etc/systemd/system/agos-proxy.service`:

```
[Unit]
Description=AGOS Proxy — AI gateway
After=network.target

[Service]
Type=exec
User=agos
Group=agos
WorkingDirectory=/var/lib/agos-proxy
Environment=AGOS_HOME=/var/lib/agos-proxy
ExecStart=/usr/local/bin/agos-proxy serve --bind 0.0.0.0:8080
Restart=on-failure
RestartSec=5
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

Adapt the user, group, working directory, and bind address to your environment.
Then:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now agos-proxy
sudo systemctl status agos-proxy
```

Logs:

```sh
journalctl -u agos-proxy -f
```

### Configuring logging in systemd

AGOS Proxy uses `tracing-subscriber`. Control verbosity with `RUST_LOG`:

```
Environment=RUST_LOG=info
```

Set it to `debug` for more detail, or `off` to silence library noise.

## Reverse proxy

AGOS Proxy does not terminate TLS itself. Put it behind a TLS-terminating
reverse proxy when exposing it on the network.

### Caddy

```
api.example.com {
    reverse_proxy localhost:8080
    tls internal
}
```

### nginx

```
server {
    listen 443 ssl;
    server_name api.example.com;

    ssl_certificate     /etc/ssl/certs/api.example.com.crt;
    ssl_certificate_key /etc/ssl/private/api.example.com.key;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # OpenAI SDKs upload large prompts; give them room.
        proxy_read_timeout 120s;
        proxy_send_timeout 120s;
    }
}
```

### Environment variables in the proxy

Remember to set `AGOS_HOME` if you want the store in a specific location:

```
Environment=AGOS_HOME=/var/lib/agos-proxy
```

## Environment variables reference

| Variable      | Purpose                                         | Default                            |
|---------------|-------------------------------------------------|------------------------------------|
| `AGOS_HOME`   | Override the data directory.                    | `$XDG_CONFIG_HOME/agos-proxy`     |
| `RUST_LOG`    | Tracing filter for the `serve` command.         | `info`                            |
| `AGOS_SETUP`  | (Docker / bootstrap only) JSON document to seed the store. | —           |

## Data directory layout

| Path                              | Purpose                                          |
|-----------------------------------|--------------------------------------------------|
| `<data-dir>/agos.db`              | SQLite store (profiles, providers, routes, usage).|
| (master key in `meta` table)      | Encryption key; not a separate file.              |

Back up the entire data directory. For portable, cross-key backup, use
`atos-proxy config export` with a passphrase instead — that produces a sealed
file that can be restored on any machine.

## Network considerations

- **Local development**: bind to `127.0.0.1` (the default). No further isolation
  is needed.
- **Single-host production**: bind to a local address and front with a reverse
  proxy that enforces access control.
- **Multi-host**: AGOS Proxy is single-process, single-store, single-machine
  today. Running multiple instances requires multiple stores and does not share
  rate limits, usage logs, or health state between them.
- **Firewall**: restrict the proxy port to the reverse proxy or VPN when
  internet-facing.

## Health checking

The proxy exposes a health endpoint:

```
GET /health
```

It returns `200 OK` when the proxy is alive. Use it for load-balancer or
orchestrator health checks.

## Backup and restore

### Backup the store

```sh
cp -a "$AGOS_HOME" /backup/agos-proxy-$(date +%Y%m%d)
```

### Portable backup and restore

```sh
# Export a sealed copy (you choose the passphrase).
atos-proxy config export --profile coder1 > coder1-sealed.json

# Restore it on another machine (re-encrypts tokens with the local master key).
atos-proxy config import --file coder1-sealed.json
```

The sealed file contains the full profile tree (providers with their upstream
tokens, proxies, routes). It is the recommended way to move a setup between
machines or to back it up independently of the raw SQLite file.

## Upgrading

AGOS Proxy uses an embedded SQLite store with forward-compatible migrations. A
new binary should open an older store and apply any needed migrations on startup.

Before upgrading:

1. Back up the data directory.
2. Read the release notes — if a migration is non-trivial, it will be documented.
3. Stop the running proxy.
4. Replace the binary (or rebuild).
5. Start the proxy — migrations run automatically.
6. Verify with `atos-proxy profile list` and a smoke request.

If a migration fails, restore the backup before proceeding.

## Downtime and restarts

The proxy is stateful in the store only. A restart:

- Preserves profiles, providers, proxies, routes, and usage history.
- Resets in-memory health state (entries start from their stored status).
- Resets in-memory rate-limit windows (they restart from zero).

A graceful restart is simply stopping and starting. The store is SQLite with WAL
mode, so commits are durable.

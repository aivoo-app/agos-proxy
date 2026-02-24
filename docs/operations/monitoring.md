# Monitoring and Observability

> How to monitor AGOS Proxy in production.

## Table of Contents

- [Logging](#logging)
- [Health Endpoint](#health-endpoint)
- [Usage Metrics](#usage-metrics)
- [Route Health](#route-health)
- [Alerting](#alerting)
- [Integration with Monitoring Systems](#integration-with-monitoring-systems)

---

## Logging

AGOS Proxy uses the `tracing` crate with `tracing-subscriber`. Logs are written to stdout.

### Log Levels

| Level | What it captures |
|-------|-----------------|
| `error` | Internal errors, upstream connection failures |
| `warn` | Health probe failures, recoverable issues |
| `info` | Startup, request handling, status changes |
| `debug` | Per-request details, routing decisions |
| `trace` | Full request/response bodies (use with caution — may log sensitive data) |

### Configure via RUST_LOG

```sh
# Production: info and above
RUST_LOG=info agos-proxy serve

# Debug routing issues
RUST_LOG=agos::router=debug agos-proxy serve

# Trace everything (verbose, may include sensitive data)
RUST_LOG=trace agos-proxy serve
```

### What gets logged

- **Server startup:** bind address, store location, migrations applied.
- **Request handling:** at `debug` level, the model string, chosen target, and outcome.
- **Health probes:** at `debug` level, each probe attempt; at `info` level, status changes.
- **Errors:** upstream connection failures, timeouts, internal errors.

### Log output format

Default format (from `tracing-subscriber`):

```
2025-01-15T10:30:00.123Z  INFO agos::server: AGOS Proxy listening on 127.0.0.1:3000
2025-01-15T10:30:05.456Z  INFO agos::router: routing request for model "Programmer/code-gen" -> deepseek/deepseek-chat
2025-01-15T10:30:05.789Z  INFO agos::router: failover: deepseek failed (timeout), trying anthropic
```

---

## Health Endpoint

```
GET /health
```

Returns `200 OK` when the server is running.

**Use for:**
- Load balancer health checks
- Kubernetes liveness probes
- systemd watchdog

Example:
```sh
curl -sf http://localhost:3000/health && echo "healthy" || echo "unhealthy"
```

---

## Usage Metrics

AGOS Proxy tracks per-request usage in the `usage_log` table. Surface it via the CLI:

### Per-model aggregates

```sh
agus-proxy usage stats --profile <name>
```

Output:
```
Usage for "coder1" (per model):
MODEL                       CALLS   FAILURES  AVG LAT(ms)    PROMPT  COMPLETION
deepseek-chat                 847          3          412      12500        8400
claude-3-5-haiku              120         12          680       3200        2100
gpt-4o                        200          5          520       5600        4800
```

Key metrics to watch:
- **CALLS:** Total request count per model.
- **FAILURES:** Count of failed attempts (indicates failover events).
- **AVG LAT(ms):** Average latency — rising latency may indicate provider issues.
- **PROMPT / COMPLETION:** Token counts for cost estimation.

### Recent requests

```sh
agus-proxy usage recent --profile <name> --limit 50
```

Output:
```
Last 50 requests for "coder1":
  [2025-01-15 10:30:00] stream model=deepseek-chat ok (200) latency=412ms tokens=45/32
  [2025-01-15 10:30:05] direct model=claude-3-5-haiku failed (503) latency=10012ms tokens=-
        error: provider returned 503: Service Unavailable
  [2025-01-15 10:30:06] direct model=gpt-4o ok (200) latency=520ms tokens=45/28
```

### Health state

```sh
agos-proxy route status --route <route>
```

Output:
```
Route "code-gen" (proxy "Programmer", strategy: Priority)
PRI   ID     STATUS    MODEL                   PROVIDE                WEIGHT
1     10     healthy   deepseek-chat            deepseek               1.0
2     11     degraded  claude-3-5-haiku         anthropic              1.0
3     12     healthy   gpt-4o                   openai                 1.0
```

**Interpretation:**
- `healthy`: In normal rotation.
- `degraded`: Recently failed; will become `unhealthy` on next probe failure.
- `unhealthy`: Skipped from live traffic; being probed.
- `disabled`: Manually turned off.

---

## Alerting

### Recommended Alerts

| Alert | Condition | Severity | Action |
|-------|-----------|----------|--------|
| **Server down** | `/health` returns non-200 or times out | Critical | Restart; check logs |
| **High failure rate** | Failure rate > 10% in last 5 minutes | Warning | Check `usage recent`; check provider status |
| **All entries unhealthy** | Route has 0 healthy entries | Critical | All providers are down; investigate |
| **Rate limit hit** | 429 responses in last 5 minutes | Info | Expected if rate limit is set; check if expected |
| **Disk usage** | `$AGOS_HOME` disk > 80% | Warning | Backup and rotate usage log |

### Prometheus Integration

AGOS Proxy does not expose a Prometheus metrics endpoint today. To integrate:

1. Parse the stdout logs with a log shipper (fluentd, vector, promtail).
2. Use the CLI commands as a metrics source:
   ```sh
   # Example: extract failure rate via usage stats
   agos-proxy usage stats --profile <name> | awk '{print $2, $3}'
   ```
3. For a proper Prometheus endpoint, open a feature request.

---

## Integration with Monitoring Systems

### Structured Logging (JSON)

AGOS Proxy uses `tracing-subscriber`'s default (human-readable) format. For JSON output, modify the `serve` function to use `tracing_subscriber::fmt().json()`. This is not configurable via env var today — open a feature request if needed.

### systemd + journald

When running under systemd, logs go to the journal:

```sh
# Follow logs
journalctl -u agos-proxy -f

# Last hour
journalctl -u agos-proxy --since "1 hour ago"

# Errors only
journalctl -u agos-proxy -p err

# Export to a file for analysis
journalctl -u agos-proxy --since "24 hours ago" > agos-logs.txt
```

### Docker

```sh
# View logs
docker compose logs -f proxy

# Since a specific time
docker compose logs --since "2025-01-15T10:00:00" proxy
```

### Log Aggregation

For production deployments, ship logs to a centralized system:

- **ELK Stack:** Use Filebeat or Fluentd to ship journald/Docker logs.
- **Grafana Loki:** Use Promtail to collect and query logs.
- **Datadog / New Relic:** Use their respective agents to collect from journald.

### Custom Metrics Dashboard

Build a simple dashboard using the CLI as a data source:

```sh
#!/bin/sh
# metrics.sh — export metrics in a dashboard-friendly format

echo "# HELP agos_calls_total Total calls per model"
echo "# TYPE agos_calls_total counter"
agos-proxy usage stats --profile production 2>/dev/null | tail -n +3 | while read model calls failures latency prompt completion; do
    echo "agos_calls_total{model=\"$model\"} $calls"
done

echo "# HELP agos_failures_total Total failures per model"
echo "# TYPE agos_failures_total counter"
agos-proxy usage stats --profile production 2>/dev/null | tail -n +3 | while read model calls failures latency prompt completion; do
    echo "agos_failures_total{model=\"$model\"} $failures"
done
```

Run this on a cron job and feed to your metrics system.

### Uptime Monitoring

Monitor the `/health` endpoint with:

- **Uptime Robot** (free tier)
- **Datadog Synthetics**
- **Grafana Cloud**
- **A simple cron job:**
  ```sh
  */5 * * * * curl -sf http://localhost:3000/health || echo "AGOS Proxy is down" | mail -s "AGOS Alert" ops@example.com
  ```

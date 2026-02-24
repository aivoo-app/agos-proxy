# Operations Runbook

> Day-2 operations guide for running AGOS Proxy in production.

## Table of Contents

- [Starting and Stopping](#starting-and-stopping)
- [Health Checks](#health-checks)
- [Backup and Restore](#backup-and-restore)
- [Upgrading](#upgrading)
- [Disaster Recovery](#disaster-recovery)
- [Incident Response](#incident-response)
- [Capacity Planning](#capacity-planning)
- [Maintenance Tasks](#maintenance-tasks)

---

## Starting and Stopping

### Start

```sh
# Foreground
agos-proxy serve --bind 127.0.0.1:3000

# Background
nohup agos-proxy serve --bind 127.0.0.1:3000 > /var/log/agos.log 2>&1 &

# systemd
sudo systemctl start agos-proxy
```

### Stop

```sh
# Foreground: Ctrl-C

# Background
kill $(pidof agos-proxy)

# systemd
sudo systemctl stop agos-proxy
```

### Verify it's running

```sh
curl http://localhost:3000/health
# Expected: 200 OK
```

---

## Health Checks

### Liveness

The `/health` endpoint returns `200 OK` when the proxy is alive:

```sh
curl -sf http://localhost:3000/health
```

Use this for:
- Load balancer health checks
- Kubernetes liveness probes
- systemd `ExecStartPost` checks

### Readiness

There is no separate readiness endpoint today. Use `/health` combined with:
- `agos-proxy profile list` returns successfully
- `agos-proxy route status` shows at least one healthy entry

### Deep Health Check

For a comprehensive health check:

```sh
#!/bin/sh
# Check the server is alive
curl -sf http://localhost:3000/health || exit 1

# Check we have at least one profile with a healthy route
HEALTHY=$(agos-proxy route status 2>/dev/null | grep -c "healthy")
[ "$HEALTHY" -gt 0 ] || exit 1

exit 0
```

---

## Backup and Restore

### Automated Backup Script

```sh
#!/bin/sh
# /etc/cron.daily/agos-proxy-backup
set -euo pipefail

BACKUP_DIR="/backup/agos-proxy/$(date +%Y%m%d)"
mkdir -p "$BACKUP_DIR"

# Copy the data directory
cp -a "$AGOS_HOME" "$BACKUP_DIR/"

# Also export each profile as sealed config
for profile in $(agos-proxy profile list --names-only 2>/dev/null); do
    agos-proxy config export --profile "$profile" --output "$BACKUP_DIR/${profile}.json"
done

# Keep only 7 days of backups
find /backup/agos-proxy -maxdepth 1 -type d -mtime +7 -exec rm -rf {} \;
```

### Restore from Backup

```sh
#!/bin/sh
# Restore from a backup directory
BACKUP_DIR="/backup/agos-proxy/20250101"

# Stop the proxy
sudo systemctl stop agos-proxy

# Restore the data directory
rm -rf "$AGOS_HOME"
cp -a "$BACKUP_DIR/agos-proxy" "$AGOS_HOME"

# Start the proxy
sudo systemctl start agos-proxy

# Verify
curl -sf http://localhost:3000/health
```

### Cross-Machine Restore

If restoring to a different machine (different master key):

```sh
# Stop the proxy
sudo systemctl stop agos-proxy

# Wipe the store (import creates a fresh one)
rm -f "$AGOS_HOME/agos.db"

# Import sealed configs
for file in /backup/*.json; do
    agos-proxy config import --path "$file"
done

# Start the proxy
sudo systemctl start agos-proxy
```

---

## Upgrading

### Upgrade Procedure

1. **Read the release notes** — check for breaking changes and migration notes.
2. **Back up your data:**
   ```sh
   cp -a "$AGOS_HOME" "/backup/agos-proxy-pre-upgrade-$(date +%Y%m%d)"
   ```
3. **Stop the proxy:**
   ```sh
   sudo systemctl stop agos-proxy
   ```
4. **Replace the binary:**
   ```sh
   sudo cp target/release/agos-proxy /usr/local/bin/agos-proxy
   sudo chmod +x /usr/local/bin/agos-proxy
   ```
5. **Start the proxy** (migrations run automatically):
   ```sh
   sudo systemctl start agos-proxy
   ```
6. **Verify:**
   ```sh
   agos-proxy --version
   curl -sf http://localhost:3000/health
   agos-proxy profile list
   ```

### Rollback

If the upgrade fails:

```sh
# Stop the proxy
sudo systemctl stop agos-proxy

# Restore the old binary
sudo cp /usr/local/bin/agos-proxy.bak /usr/local/bin/agos-proxy

# If the store was migrated, restore from backup
rm -rf "$AGOS_HOME"
cp -a "/backup/agos-proxy-pre-upgrade" "$AGOS_HOME"

# Start the proxy
sudo systemctl start agos-proxy
```

---

## Disaster Recovery

### Scenario: Data directory lost

If the data directory is lost and you have a sealed config export:

```sh
# Create a fresh data directory
mkdir -p "$AGOS_HOME"

# Import each profile
for file in /secure-backup/*.json; do
    agos-proxy config import --path "$file"
done
```

If you do NOT have a sealed export, provider tokens are lost. You'll need to:
1. Recreate profiles.
2. Re-add all providers with their API tokens.
3. Recreate proxies and routes.

### Scenario: Server hardware failure

1. Provision a new server.
2. Install AGOS Proxy.
3. Copy the data directory from your backup.
4. Start the server.
5. Update DNS/reverse proxy to point to the new server.

### Scenario: Store file corruption

```sh
# Check if SQLite can read the file
sqlite3 "$AGOS_HOME/agos.db" "PRAGMA integrity_check;"

# If corrupted and you have a backup, restore it
sqlite3 "$AGOS_HOME/agos.db" ".dump" > /tmp/recover.sql  # may fail

# Otherwise, restore from backup
cp -a /backup/agos-proxy/latest/* "$AGOS_HOME/"
```

---

## Incident Response

### High Error Rate

**Symptom:** Many requests failing.

**Steps:**
1. Check recent usage:
   ```sh
   agos-proxy usage recent --profile <name> --limit 50
   ```
2. Check route health:
   ```sh
   agos-proxy route status --route <route>
   ```
3. Check if it's a specific provider (all errors from one provider) or all providers.
4. If one provider is down, consider disabling its entries:
   ```sh
   agos-proxy route model disable --proxy <proxy> --route <route> --model <model>
   ```
5. Check the provider's status page.

### Rate Limit Storm

**Symptom:** 429 errors across all profiles.

**Steps:**
1. Identify the source: `agos-proxy usage recent` for each profile.
2. If one profile is hammering the API, lower its rate limit:
   ```sh
   agos-proxy profile limit <name> <lower-rpm>
   ```
3. If it's a shared upstream provider hitting its own rate limit, add more providers to the chains.

### Security Incident

**Suspicion that a provider token or profile token has been leaked:**

1. **Rotate the profile token immediately:**
   ```sh
   agos-proxy profile token rotate <name>
   ```
2. **Update all clients with the new token.**
3. **If a provider token was leaked:**
   ```sh
   agos-proxy provider edit --profile <name>
   ```
4. **Revoke the old token at the provider** (if the provider supports it).
5. **Check the usage log for unauthorized requests:**
   ```sh
   agos-proxy usage recent --profile <name> --limit 100
   ```

---

## Capacity Planning

### Disk Space

The store grows with:
- **Profiles/providers/proxies/routes:** Negligible (KB).
- **Usage log:** ~1 KB per request. 1M requests ≈ 1 GB.
- **Route entries:** Negligible.

For high-volume deployments, monitor disk usage and implement log rotation.

### Memory

- Base: ~20 MB for the process.
- Per-connection: negligible (async).
- Per-profile rate limiter: stores timestamps in memory (up to `rpm_limit` entries per profile).

### Throughput

- AGOS Proxy itself handles thousands of concurrent connections.
- The bottleneck is upstream provider latency and rate limits.
- For horizontal scaling, run multiple instances with separate `AGOS_HOME` directories.

---

## Maintenance Tasks

### Daily

- [ ] Check `/health` endpoint responds.
- [ ] Review error rate in `usage stats`.

### Weekly

- [ ] Check disk usage of `$AGOS_HOME`.
- [ ] Review `usage stats` for unexpected patterns.
- [ ] Verify route health: `route status`.

### Monthly

- [ ] Back up the data directory.
- [ ] Test restore procedure on a staging instance.
- [ ] Review and rotate any tokens that may have been exposed.
- [ ] Check for AGOS Proxy updates.

### Quarterly

- [ ] Review rate limits — are they still appropriate?
- [ ] Review provider chains — any providers to add/remove?
- [ ] Audit profile access — any unused profiles to clean up?

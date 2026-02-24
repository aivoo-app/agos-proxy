# Store Migration Guide

> How AGOS Proxy handles store schema changes and how to migrate between versions.

## Table of Contents

- [How Migrations Work](#how-migrations-work)
- [Migration History](#migration-history)
- [Manual Migration](#manual-migration)
- [Cross-Machine Migration](#cross-machine-migration)
- [Troubleshooting Migrations](#troubleshooting-migrations)

---

## How Migrations Work

AGOS Proxy uses an embedded SQLite store with forward-compatible migrations. When the store is opened:

1. **Schema creation:** `CREATE TABLE IF NOT EXISTS` statements run for all tables. This is idempotent — existing tables are untouched.
2. **Column migrations:** The `migrate_columns` function checks for columns added after the store was created and adds them via `ALTER TABLE ADD COLUMN`.
3. **Data preservation:** Migrations only add columns or tables. They never remove or rename existing data.

This means:
- An older store opens correctly with a newer binary.
- Migrations run automatically on startup — no manual intervention needed.
- Data is preserved across upgrades.

### Migration Mechanism

```rust
// From src/storage/schema.rs
pub fn migrate_columns(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    ensure_column(conn, "profiles", "rpm_limit", "INTEGER NOT NULL DEFAULT 0")?;
    Ok(())
}
```

Each `ensure_column` call:
1. Checks `PRAGMA table_info(table)` for the column.
2. If missing, runs `ALTER TABLE table ADD COLUMN column decl`.

---

## Migration History

### v0.1.0 → v0.1.0 (initial)

| Change | Table | Column | Type | Default |
|--------|-------|--------|------|---------|
| Initial schema | `profiles` | `id` | TEXT PK | — |
| | `profiles` | `name` | TEXT UNIQUE | — |
| | `profiles` | `description` | TEXT | NULL |
| | `profiles` | `password_hash` | TEXT | NULL |
| | `profiles` | `created_at` | INTEGER | — |
| | `profiles` | `updated_at` | INTEGER | — |
| | `profiles` | `rpm_limit` | INTEGER | 0 |
| | `providers` | (all columns) | — | — |
| | `proxies` | (all columns) | — | — |
| | `routes` | (all columns) | — | — |
| | `route_entries` | (all columns) | — | — |
| | `usage_log` | (all columns) | — | — |
| | `meta` | (all columns) | — | — |

### Future Migrations

When a new column is added in a future version:

1. The column is added to `migrate_columns` in `src/storage/schema.rs`.
2. The column is added to the `CREATE TABLE IF NOT EXISTS` statement (for new stores).
3. Existing stores get the column added on next open.

---

## Manual Migration

### When to Migrate Manually

- You copied the SQLite file to a machine with a different master key.
- You need to move data between stores with different encryption keys.
- The automatic migration failed (rare).

### Export/Import Migration

The recommended way to move data between machines or stores:

```sh
# Export from source
agos-proxy config export --profile <name> --output backup.json

# Import to target
agos-proxy config import --path backup.json
```

This:
1. Reads the full profile tree from the source store.
2. Seals it with a passphrase.
3. On import, re-encrypts provider tokens with the destination store's master key.
4. Issues a fresh bearer token for the imported profile.

### Bulk Export

To export all profiles:

```sh
#!/bin/sh
for profile in $(agos-proxy profile list --names-only 2>/dev/null); do
    agos-proxy config export --profile "$profile" --output "${profile}.json"
done
```

### Bulk Import

```sh
#!/bin/sh
for file in *.json; do
    agos-proxy config import --path "$file"
done
```

---

## Cross-Machine Migration

### Scenario: Moving to a new server

1. **On the old server:**
   ```sh
   agos-proxy config export --profile production --output production.json
   ```

2. **Transfer the file securely:**
   ```sh
   scp production.json user@new-server:/secure/path/
   ```

3. **On the new server:**
   ```sh
   # Install AGOS Proxy
   # Create the data directory
   mkdir -p "$AGOS_HOME"

   # Import
   agos-proxy config import --path production.json
   ```

4. **Verify:**
   ```sh
   agos-proxy profile list
   agos-proxy route status
   ```

5. **Update clients** with the new bearer token (printed at import time).

### Scenario: Migrating from SQLite file copy (wrong way)

If you copied the SQLite file directly (not via export/import):

```sh
# This will fail with "decryption failed" because the master key is different
agos-proxy profile list
# Error: decryption failed (wrong key or corrupted data)
```

**Fix:** Restore from a sealed export, or re-create the profile and re-enter provider tokens.

---

## Troubleshooting Migrations

### "no such column" errors

**Symptom:** Errors referencing a missing column.

**Cause:** The store was created with an older schema and the migration didn't run.

**Fix:** The migration should run automatically. If it doesn't:
1. Check the version of the binary: `agos-proxy --version`.
2. Ensure you're running the latest version.
3. If the issue persists, export and re-import the profile.

### "database is malformed"

**Symptom:** SQLite reports a malformed database.

**Cause:** The file was corrupted (incomplete write, disk error, etc.).

**Fix:**
```sh
# Try to recover what you can
sqlite3 agos.db ".dump" > recovery.sql

# Or restore from backup
cp /backup/agos-proxy/latest/agos.db "$AGOS_HOME/agos.db"
```

### Migration takes a long time

**Cause:** Large `usage_log` table with millions of rows.

**Fix:** The migration itself is fast (it only checks for column existence). If the store is slow to open, it may be due to WAL checkpointing. This is normal for large stores.

### After migration, some data is missing

**Cause:** Migrations only add columns — they don't remove data. If data is missing, it was likely deleted before the migration.

**Fix:** Restore from a sealed export or backup.

---

## Best Practices

1. **Always use `config export`/`config import`** for cross-machine migration. Never copy the raw SQLite file.
2. **Back up before upgrading.** Even though migrations are safe, a backup is your safety net.
3. **Test upgrades on staging** before applying to production.
4. **Keep sealed exports in a secure location** (encrypted at rest, access-controlled).
5. **Document your store location** (`AGOS_HOME`) so you can find it for backups.

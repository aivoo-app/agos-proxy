//! SQLite schema for the domain model.
//!
//! Migrations are applied idempotently on store open. `CREATE IF NOT EXISTS`
//! keeps schema evolutions painless for a local single-file database, and
//! column additions are applied by [`migrate_columns`]. The `route_entries`
//! table is the one exception: making `provider_id` nullable (so an entry can
//! reference another route) requires a full table rebuild, handled by
//! [`migrate_route_entries`].

/// The `route_entries` table body: column and constraint definitions without
/// the `CREATE TABLE` wrapper.
///
/// Kept as its own constant because the nested-route migration must build a
/// fresh copy of the table with exactly this shape — `CREATE TABLE IF NOT
/// EXISTS` cannot evolve an existing table, and `provider_id` had to become
/// nullable so an entry can point at another route instead of a provider.
pub const ROUTE_ENTRIES_BODY: &str = "
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        route_id        INTEGER NOT NULL REFERENCES routes(id) ON DELETE CASCADE,
        provider_id     INTEGER REFERENCES providers(id) ON DELETE CASCADE,
        target_route_id INTEGER REFERENCES routes(id) ON DELETE CASCADE,
        model_id        TEXT NOT NULL,
        priority        INTEGER NOT NULL,
        weight          REAL NOT NULL DEFAULT 1.0,
        status          TEXT NOT NULL,
        capabilities    TEXT NOT NULL,
        price_per_1m    REAL NOT NULL DEFAULT 0.0,
        cooldown_until  INTEGER NOT NULL DEFAULT 0,
        -- Exactly one target kind per entry: a real provider/model pair, or a
        -- reference to another route (route-as-model).
        CHECK (
            (provider_id IS NOT NULL AND target_route_id IS NULL)
            OR (provider_id IS NULL AND target_route_id IS NOT NULL)
        )
    ";

/// Idempotent DDL that establishes the schema.
pub const SCHEMA_HEAD: &str = "
    PRAGMA journal_mode = WAL;
    PRAGMA foreign_keys = ON;
    PRAGMA cache_size = -65536;
    PRAGMA mmap_size = 268435456;
    PRAGMA synchronous = NORMAL;
    PRAGMA temp_store = MEMORY;

    CREATE TABLE IF NOT EXISTS meta (
        key   TEXT PRIMARY KEY,
        value BLOB NOT NULL
    );

    CREATE TABLE IF NOT EXISTS profiles (
        id            TEXT PRIMARY KEY,
        name          TEXT NOT NULL UNIQUE,
        description   TEXT,
        password_hash TEXT,
        created_at    INTEGER NOT NULL,
        updated_at    INTEGER NOT NULL,
        rpm_limit     INTEGER NOT NULL DEFAULT 0
    );

    CREATE TABLE IF NOT EXISTS providers (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        profile_id    TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
        name          TEXT NOT NULL,
        description   TEXT,
        base_url      TEXT NOT NULL,
        auth_token    BLOB NOT NULL,
        kind          TEXT NOT NULL,
        extra_headers TEXT NOT NULL,
        shared        INTEGER NOT NULL DEFAULT 0
    );

    -- Egress masks: HTTP hops that upstream requests are sent through, so a
    -- provider's traffic leaves from a different network identity than the
    -- host running AGOS. Bound at the provider level (see
    -- `providers.masking_server_id`) so each key can present its own identity.
    CREATE TABLE IF NOT EXISTS masking_servers (
        id                  INTEGER PRIMARY KEY AUTOINCREMENT,
        profile_id          TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
        name                TEXT NOT NULL,
        kind                TEXT NOT NULL,
        endpoint_url        TEXT NOT NULL,
        secret              BLOB NOT NULL,
        max_body_bytes      INTEGER NOT NULL DEFAULT 0,
        expected_egress_ip  TEXT,
        last_verified_ip    TEXT,
        last_verified_asn   TEXT,
        last_verified_country TEXT,
        last_verified_at    INTEGER,
        shared              INTEGER NOT NULL DEFAULT 0
    );

    CREATE UNIQUE INDEX IF NOT EXISTS idx_masks_profile ON masking_servers(profile_id, name);

    CREATE TABLE IF NOT EXISTS proxies (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        profile_id  TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
        name        TEXT NOT NULL,
        description TEXT
    );

    CREATE TABLE IF NOT EXISTS routes (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        proxy_id    INTEGER NOT NULL REFERENCES proxies(id) ON DELETE CASCADE,
        name        TEXT NOT NULL,
        description TEXT,
        strategy    TEXT NOT NULL,
        identity    TEXT,
        max_tokens  INTEGER NOT NULL DEFAULT 0,
        cache_ttl_secs INTEGER NOT NULL DEFAULT 0,
        shared      INTEGER NOT NULL DEFAULT 0,
        prompt_cache TEXT NOT NULL DEFAULT 'auto'
    );

    CREATE UNIQUE INDEX IF NOT EXISTS idx_providers_profile ON providers(profile_id, name);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_proxies_profile    ON proxies(profile_id, name);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_routes_proxy       ON routes(proxy_id, name);

    CREATE TABLE IF NOT EXISTS usage_log (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        profile_id    TEXT NOT NULL,
        route_entry_id INTEGER NOT NULL REFERENCES route_entries(id) ON DELETE CASCADE,
        model_id      TEXT NOT NULL,
        streamed      INTEGER NOT NULL DEFAULT 0,
        success       INTEGER NOT NULL,
        status_code   INTEGER,
        error_message TEXT,
        latency_ms    INTEGER NOT NULL,
        prompt_tokens INTEGER,
        completion_tokens INTEGER,
        cached_prompt_tokens INTEGER,
        created_at    INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_usage_profile ON usage_log(profile_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_usage_entry   ON usage_log(route_entry_id, created_at);

    -- Economy cache: deterministic exact-match responses (0 upstream tokens).
    CREATE TABLE IF NOT EXISTS response_cache (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        route_id    INTEGER NOT NULL REFERENCES routes(id) ON DELETE CASCADE,
        req_hash    TEXT NOT NULL,
        resp_json   BLOB NOT NULL,
        prompt_tokens INTEGER,
        completion_tokens INTEGER,
        created_at  INTEGER NOT NULL,
        expires_at  INTEGER NOT NULL
    );
    CREATE UNIQUE INDEX IF NOT EXISTS idx_cache_route_hash ON response_cache(route_id, req_hash);
    CREATE INDEX IF NOT EXISTS idx_cache_expiry ON response_cache(expires_at);

";

/// The full DDL, with the canonical `route_entries` body spliced in.
pub fn schema_sql() -> String {
    format!("{SCHEMA_HEAD}    CREATE TABLE IF NOT EXISTS route_entries ({ROUTE_ENTRIES_BODY});\n")
}

/// Bring stores created before a given column existed up to date.
///
/// `CREATE TABLE IF NOT EXISTS` cannot evolve an existing table, so column
/// additions are checked and applied one at a time on every open.
pub fn migrate_columns(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    ensure_column(conn, "profiles", "rpm_limit", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(conn, "routes", "identity", "TEXT")?;
    ensure_column(conn, "routes", "max_tokens", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(
        conn,
        "routes",
        "cache_ttl_secs",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "route_entries",
        "price_per_1m",
        "REAL NOT NULL DEFAULT 0.0",
    )?;

    // Egress masking. Both references use ON DELETE SET NULL so removing a mask
    // only unbinds it — it never cascades into providers or profiles.
    ensure_column(
        conn,
        "providers",
        "masking_server_id",
        "INTEGER REFERENCES masking_servers(id) ON DELETE SET NULL",
    )?;
    ensure_column(
        conn,
        "profiles",
        "default_masking_server_id",
        "INTEGER REFERENCES masking_servers(id) ON DELETE SET NULL",
    )?;
    // Unix-millis instant until which a rate-limited entry is skipped by the
    // router. 0 means "not cooling".
    ensure_column(
        conn,
        "route_entries",
        "cooldown_until",
        "INTEGER NOT NULL DEFAULT 0",
    )?;

    // Cross-profile resource sharing: 1 = the resource is published to the
    // whole instance and may be referenced from any profile's route chain.
    ensure_column(conn, "providers", "shared", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(
        conn,
        "masking_servers",
        "shared",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "routes", "shared", "INTEGER NOT NULL DEFAULT 0")?;
    // Provider-native prompt-cache policy for this route (`auto` | `off`).
    ensure_column(
        conn,
        "routes",
        "prompt_cache",
        "TEXT NOT NULL DEFAULT 'auto'",
    )?;

    // Input tokens the upstream served from its prompt cache. Nullable rather
    // than defaulted so history logged before this column existed stays
    // distinguishable from a genuine zero-cache-read call.
    ensure_column(conn, "usage_log", "cached_prompt_tokens", "INTEGER")?;

    // Normalize any route entry status tags that are no longer valid in the
    // current enum (e.g. deprecated "draining") so the store can be read without
    // crashing. Unknown statuses are migrated to "unhealthy".
    conn.execute_batch(
        "UPDATE route_entries SET status = 'unhealthy' \
         WHERE status NOT IN ('healthy', 'degraded', 'unhealthy', 'disabled')",
    )?;
    Ok(())
}

/// Rebuild `route_entries` with the nested-route shape when the store predates
/// it (a nullable `provider_id` plus `target_route_id` cannot be reached with
/// `ALTER TABLE ADD COLUMN`, which can neither null a column nor add a CHECK).
///
/// **The hazard this migration is written around:** `usage_log.route_entry_id`
/// references `route_entries(id) ON DELETE CASCADE`, so dropping the old table
/// with foreign keys enabled would silently erase the entire usage history.
/// The rebuild therefore runs with `PRAGMA foreign_keys = OFF`, copies every
/// row, and restores enforcement afterwards. It is a no-op on stores that
/// already have the new shape, and it never touches the CHECK-mandated
/// invariants (old rows all had a provider).
pub fn migrate_route_entries(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let present: bool = conn
        .prepare("PRAGMA table_info(route_entries)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .any(|name| name == "target_route_id");
    if present {
        return Ok(());
    }

    // Foreign keys must be toggled outside a transaction to take effect.
    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
    let rebuild = format!(
        "BEGIN;
         CREATE TABLE route_entries_rebuilt ({});
         INSERT INTO route_entries_rebuilt
             (id, route_id, provider_id, target_route_id, model_id, priority,
              weight, status, capabilities, price_per_1m, cooldown_until)
         SELECT id, route_id, provider_id, NULL, model_id, priority,
                weight, status, capabilities, price_per_1m, cooldown_until
         FROM route_entries;
         DROP TABLE route_entries;
         ALTER TABLE route_entries_rebuilt RENAME TO route_entries;
         COMMIT;",
        ROUTE_ENTRIES_BODY
    );
    if let Err(e) = conn.execute_batch(&rebuild) {
        // Leave the store consistent: drop any half-built copy and restore
        // enforcement before surfacing the failure.
        let _ = conn.execute_batch(
            "ROLLBACK;
             DROP TABLE IF EXISTS route_entries_rebuilt;
             PRAGMA foreign_keys = ON;",
        );
        return Err(anyhow::anyhow!(e).context("rebuilding route_entries"));
    }
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    Ok(())
}

fn ensure_column(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
    decl: &str,
) -> anyhow::Result<()> {
    let present: bool = conn
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .any(|name| name == column);
    if !present {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a store in the pre-nested-route shape: `route_entries.provider_id`
    /// was `NOT NULL` and there was no `target_route_id`.
    fn legacy_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE profiles (
                 id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, description TEXT,
                 password_hash TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                 rpm_limit INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE providers (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
                 name TEXT NOT NULL, description TEXT, base_url TEXT NOT NULL,
                 auth_token BLOB NOT NULL, kind TEXT NOT NULL, extra_headers TEXT NOT NULL
             );
             CREATE TABLE proxies (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
                 name TEXT NOT NULL, description TEXT
             );
             CREATE TABLE masking_servers (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
                 name TEXT NOT NULL, kind TEXT NOT NULL, endpoint_url TEXT NOT NULL,
                 secret BLOB NOT NULL, max_body_bytes INTEGER NOT NULL DEFAULT 0,
                 expected_egress_ip TEXT, last_verified_ip TEXT, last_verified_asn TEXT,
                 last_verified_country TEXT, last_verified_at INTEGER
             );
             CREATE TABLE routes (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 proxy_id INTEGER NOT NULL REFERENCES proxies(id) ON DELETE CASCADE,
                 name TEXT NOT NULL, description TEXT, strategy TEXT NOT NULL,
                 identity TEXT, max_tokens INTEGER NOT NULL DEFAULT 0,
                 cache_ttl_secs INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE route_entries (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 route_id INTEGER NOT NULL REFERENCES routes(id) ON DELETE CASCADE,
                 provider_id INTEGER NOT NULL REFERENCES providers(id),
                 model_id TEXT NOT NULL, priority INTEGER NOT NULL,
                 weight REAL NOT NULL DEFAULT 1.0, status TEXT NOT NULL,
                 capabilities TEXT NOT NULL, price_per_1m REAL NOT NULL DEFAULT 0.0
             );
             CREATE TABLE usage_log (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 profile_id TEXT NOT NULL,
                 route_entry_id INTEGER NOT NULL REFERENCES route_entries(id) ON DELETE CASCADE,
                 model_id TEXT NOT NULL, streamed INTEGER NOT NULL DEFAULT 0,
                 success INTEGER NOT NULL, status_code INTEGER, error_message TEXT,
                 latency_ms INTEGER NOT NULL, prompt_tokens INTEGER,
                 completion_tokens INTEGER, created_at INTEGER NOT NULL
             );
             INSERT INTO profiles VALUES ('p1', 'legacy', NULL, NULL, 1, 1, 0);
             INSERT INTO providers VALUES (1, 'p1', 'up', NULL, 'https://up.test', x'00', 'openai', '{}');
             INSERT INTO proxies VALUES (1, 'p1', 'prog', NULL);
             INSERT INTO routes VALUES (1, 1, 'r1', NULL, 'priority', NULL, 0, 0);
             INSERT INTO route_entries VALUES (1, 1, 1, 'm1', 1, 1.0, 'Healthy', '{}', 0.0);
             INSERT INTO usage_log VALUES (1, 'p1', 1, 'm1', 0, 1, 200, NULL, 12, 5, 6, 42);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn route_entries_rebuild_keeps_usage_history() {
        let conn = legacy_db();
        // Mirror the real open order: `Store::open` runs the additive column
        // migrations before the rebuild, and the rebuild's SELECT depends on
        // those columns (e.g. `cooldown_until`) already existing.
        migrate_columns(&conn).unwrap();
        migrate_route_entries(&conn).unwrap();

        // New shape: `target_route_id` exists and `provider_id` became nullable.
        let columns: Vec<(String, bool)> = conn
            .prepare("PRAGMA table_info(route_entries)")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, i64>(3)? != 0))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(
            columns.iter().any(|(n, _)| n == "target_route_id"),
            "target_route_id must be added by the rebuild"
        );
        assert_eq!(
            columns
                .iter()
                .find(|(n, _)| n == "provider_id")
                .map(|(_, notnull)| *notnull),
            Some(false),
            "provider_id must become nullable so nested entries can omit it"
        );

        // The hazard this migration exists to avoid: dropping route_entries with
        // foreign keys enabled would have cascaded this row away.
        let usage: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_log", [], |row| row.get(0))
            .unwrap();
        assert_eq!(usage, 1, "usage history must survive the table rebuild");

        // The single existing entry was copied, with no nested reference.
        let (entry_id, provider_id, target): (i64, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT id, provider_id, target_route_id FROM route_entries",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((entry_id, provider_id, target), (1, Some(1), None));

        // Enforcement is restored, and a second run is a no-op.
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fk, 1, "foreign key enforcement must be back on");
        migrate_route_entries(&conn).unwrap();
        let usage: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_log", [], |row| row.get(0))
            .unwrap();
        assert_eq!(usage, 1, "re-running the migration must not touch data");
    }

    #[test]
    fn nested_entries_may_omit_the_provider() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(schema_sql().as_str()).unwrap();

        // A direct entry and a nested entry are both accepted...
        conn.execute_batch(
            "INSERT INTO profiles VALUES ('p', 'p', NULL, NULL, 1, 1, 0);
             INSERT INTO providers VALUES (1, 'p', 'up', NULL, 'https://up.test', x'00', 'openai', '{}', 0);
             INSERT INTO proxies VALUES (1, 'p', 'prog', NULL);
             INSERT INTO routes VALUES (1, 1, 'r1', NULL, 'priority', NULL, 0, 0, 0, 'auto');
             INSERT INTO routes VALUES (2, 1, 'r2', NULL, 'priority', NULL, 0, 0, 0, 'auto');
             INSERT INTO route_entries (route_id, provider_id, target_route_id, model_id, priority, weight, status, capabilities)
                 VALUES (1, 1, NULL, 'm1', 1, 1.0, 'Healthy', '{}');
             INSERT INTO route_entries (route_id, provider_id, target_route_id, model_id, priority, weight, status, capabilities)
                 VALUES (1, NULL, 2, 'prog/r2', 2, 1.0, 'Healthy', '{}');",
        )
        .unwrap();

        // ...but an entry naming both, or neither, violates the CHECK.
        assert!(conn
            .execute_batch(
                "INSERT INTO route_entries (route_id, provider_id, target_route_id, model_id, priority, weight, status, capabilities)
                     VALUES (1, 1, 2, 'both', 3, 1.0, 'Healthy', '{}');"
            )
            .is_err());
        assert!(conn
            .execute_batch(
                "INSERT INTO route_entries (route_id, provider_id, target_route_id, model_id, priority, weight, status, capabilities)
                     VALUES (1, NULL, NULL, 'neither', 4, 1.0, 'Healthy', '{}');"
            )
            .is_err());
    }
}

//! SQLite schema for the domain model.
//!
//! Migrations are applied idempotently on store open. `CREATE IF NOT EXISTS`
//! keeps schema evolutions painless for a local single-file database.

/// Idempotent DDL that establishes the schema.
pub const SCHEMA: &str = "
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
        extra_headers TEXT NOT NULL
    );

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
        cache_ttl_secs INTEGER NOT NULL DEFAULT 0
    );

    CREATE TABLE IF NOT EXISTS route_entries (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        route_id     INTEGER NOT NULL REFERENCES routes(id) ON DELETE CASCADE,
        provider_id  INTEGER NOT NULL REFERENCES providers(id),
        model_id     TEXT NOT NULL,
        priority     INTEGER NOT NULL,
        weight       REAL NOT NULL DEFAULT 1.0,
        status       TEXT NOT NULL,
        capabilities TEXT NOT NULL,
        price_per_1m REAL NOT NULL DEFAULT 0.0
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

    // Normalize any route entry status tags that are no longer valid in the
    // current enum (e.g. deprecated "draining") so the store can be read without
    // crashing. Unknown statuses are migrated to "unhealthy".
    conn.execute_batch(
        "UPDATE route_entries SET status = 'unhealthy' \
         WHERE status NOT IN ('healthy', 'degraded', 'unhealthy', 'disabled')",
    )?;
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

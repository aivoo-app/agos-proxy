//! SQLite schema for the domain model.
//!
//! Migrations are applied idempotently on store open. `CREATE IF NOT EXISTS`
//! keeps schema evolutions painless for a local single-file database.

/// Idempotent DDL that establishes the schema.
pub const SCHEMA: &str = "
    PRAGMA journal_mode = WAL;
    PRAGMA foreign_keys = ON;

    CREATE TABLE IF NOT EXISTS profiles (
        id            TEXT PRIMARY KEY,
        name          TEXT NOT NULL UNIQUE,
        description   TEXT,
        password_hash TEXT,
        created_at    INTEGER NOT NULL,
        updated_at    INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS providers (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        profile_id    TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
        name          TEXT NOT NULL,
        description   TEXT,
        base_url      TEXT NOT NULL,
        auth_token    TEXT NOT NULL,
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
        strategy    TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS route_entries (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        route_id     INTEGER NOT NULL REFERENCES routes(id) ON DELETE CASCADE,
        provider_id  INTEGER NOT NULL REFERENCES providers(id),
        model_id     TEXT NOT NULL,
        priority     INTEGER NOT NULL,
        weight       REAL NOT NULL DEFAULT 1.0,
        status       TEXT NOT NULL,
        capabilities TEXT NOT NULL
    );

    CREATE UNIQUE INDEX IF NOT EXISTS idx_providers_profile ON providers(profile_id, name);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_proxies_profile    ON proxies(profile_id, name);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_routes_proxy       ON routes(proxy_id, name);
";

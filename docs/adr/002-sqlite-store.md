# ADR 002: Why SQLite for Persistence

> Status: Accepted

## Context

AGOS Proxy needs a persistent store for:
- Profiles, providers, proxies, routes, route entries
- Usage log (per-request records)
- Master key for encryption

The store must be:
- Embedded (no external database to run)
- Single-file (easy to back up and migrate)
- Reliable (ACID transactions)
- Zero-configuration

## Decision

We chose SQLite (via `rusqlite` with bundled SQLite) as the persistence layer.

## Consequences

### Positive

- **Zero configuration:** No database server to install, run, or monitor.
- **Single file:** The entire store is one file (`agos.db`). Easy to back up, copy, and migrate.
- **ACID transactions:** Reliable writes even on crash.
- **WAL mode:** Readers don't block writers, writers don't block readers.
- **Mature:** SQLite is the most widely deployed database engine in the world.
- **Bundled:** The `rusqlite` crate with `bundled` feature compiles SQLite from source — no system dependency.

### Negative

- **Single writer:** SQLite allows only one writer at a time. High write concurrency requires careful design.
- **Synchronous I/O:** `rusqlite` uses synchronous I/O, which must be wrapped in `tokio::task::spawn_blocking` in async code.
- **No horizontal scaling:** SQLite is a local file. Multi-node setups require separate stores.
- **Migration limitations:** `ALTER TABLE` can only add columns, not remove or rename them.

### How We Mitigate the Negatives

- **Single writer:** AGOS Proxy is a single process. The HTTP server serializes writes through a single `Store` instance behind `Arc<Mutex<>>` for thread safety.
- **Synchronous I/O:** The HTTP handlers wrap store calls in `spawn_blocking`. Reads can use `spawn_blocking` with a shared connection.
- **No horizontal scaling:** This is a known limitation. For multi-node setups, run separate instances with separate stores.
- **Migration limitations:** We use `CREATE TABLE IF NOT EXISTS` for tables and `ALTER TABLE ADD COLUMN` for new columns. We never remove or rename columns.

## Alternatives Considered

| Store | Pros | Cons |
|-------|------|------|
| **PostgreSQL** | Full SQL, concurrent writes, mature | Requires a running server, operational overhead |
| **RocksDB / sled** | Embedded, fast, concurrent writes | No SQL, harder to query, less familiar |
| **JSON files** | Human-readable, simple | No transactions, slow queries, no concurrency |
| **SQLite (chosen)** | Zero-config, single-file, ACID, SQL | Single writer, sync I/O |

SQLite was chosen because AGOS Proxy is a single-user, single-process tool. The simplicity of a zero-config, single-file store outweighs the lack of concurrent writes.

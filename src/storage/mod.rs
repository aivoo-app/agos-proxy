//! Persistence for the domain model.
//!
//! The MVP backs onto a single embedded SQLite file, so there is no external
//! database to run or configure. This module owns the store location and the
//! connection lifecycle so the rest of the crate never has to think about where
//! data actually lives on disk.
//!
//! The store is intentionally synchronous. For the local, single-user tool that
//! is a deliberate trade: it keeps every read/write trivial to reason about.
//! Long-running async code that must not block a reactor thread (the HTTP server)
//! wraps calls in `tokio::task::spawn_blocking`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Context as _, Result};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension};

use crate::crypto::MasterKey;

use crate::domain::{
    ModelStatus, Profile, Provider, ProviderKind, Proxy, Route, RouteCapabilities, RouteEntry,
    RoutingStrategy, UsageRecord, UsageStats,
};

/// Intermediate provider row, used to defer decryption out of the rusqlite closure.
struct RawProvider {
    id: i64,
    profile_id: String,
    name: String,
    description: Option<String>,
    base_url: String,
    enc_token: Vec<u8>,
    kind_tag: String,
    extra_json: String,
}

mod schema;

// --- enum <-> string mapping -------------------------------------------------
// The wire/DB representation is a short, stable tag rather than the Rust variant
// name, so shuffled enum ordering never leaks into stored data.

fn provider_kind_tag(k: ProviderKind) -> &'static str {
    match k {
        ProviderKind::OpenAICompatible => "openai",
        ProviderKind::Anthropic => "anthropic",
        ProviderKind::Google => "google",
        ProviderKind::Custom => "custom",
    }
}

fn provider_kind_from_tag(tag: &str) -> Result<ProviderKind> {
    match tag {
        "openai" => Ok(ProviderKind::OpenAICompatible),
        "anthropic" => Ok(ProviderKind::Anthropic),
        "google" => Ok(ProviderKind::Google),
        "custom" => Ok(ProviderKind::Custom),
        _ => bail!("unknown provider kind tag {tag:?}"),
    }
}

fn strategy_tag(s: RoutingStrategy) -> &'static str {
    match s {
        RoutingStrategy::Priority => "priority",
        RoutingStrategy::RoundRobin => "round_robin",
        RoutingStrategy::Weighted => "weighted",
    }
}

fn strategy_from_tag(tag: &str) -> Result<RoutingStrategy> {
    match tag {
        "priority" => Ok(RoutingStrategy::Priority),
        "round_robin" => Ok(RoutingStrategy::RoundRobin),
        "weighted" => Ok(RoutingStrategy::Weighted),
        _ => bail!("unknown routing strategy tag {tag:?}"),
    }
}

fn status_tag(s: ModelStatus) -> &'static str {
    match s {
        ModelStatus::Healthy => "healthy",
        ModelStatus::Degraded => "degraded",
        ModelStatus::Unhealthy => "unhealthy",
        ModelStatus::Disabled => "disabled",
    }
}

fn status_from_tag(tag: &str) -> Result<ModelStatus> {
    match tag {
        "healthy" => Ok(ModelStatus::Healthy),
        "degraded" => Ok(ModelStatus::Degraded),
        "unhealthy" => Ok(ModelStatus::Unhealthy),
        "disabled" => Ok(ModelStatus::Disabled),
        _ => bail!("unknown model status tag {tag:?}"),
    }
}

/// A random profile token, hex-encoded (128 bits of entropy).
fn fresh_token() -> Result<String> {
    let a = getrandom::u64()?;
    let b = getrandom::u64()?;
    Ok(std::fmt::format(format_args!("{a:x}{b:x}")))
}

fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}

/// Details needed to create a new [`Provider`]. Kept in one place so the storage
/// API stays small and callers build the value explicitly.
pub struct NewProvider {
    pub name: String,
    pub description: Option<String>,
    pub base_url: String,
    pub auth_token: String,
    pub kind: ProviderKind,
    pub extra_headers: std::collections::BTreeMap<String, String>,
}

/// Details needed to append one request to the usage log.
pub struct NewUsage {
    pub profile_id: String,
    pub route_entry_id: i64,
    pub model_id: String,
    pub streamed: bool,
    pub success: bool,
    pub status_code: Option<i32>,
    pub error_message: Option<String>,
    pub latency_ms: i64,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
}

/// Handle to the on-disk store.
///
/// The connection is wrapped in a `Mutex` so the store can be shared across
/// threads (e.g. the HTTP server's async handlers). Every method locks, does
/// its work, and returns — no lock is ever held across an await point.
pub struct Store {
    conn: Mutex<Connection>,
    path: PathBuf,
    master_key: Mutex<Option<MasterKey>>,
}

impl Store {
    /// Lock the connection and return a guard. Centralizes the poisoning
    /// handling so every caller doesn't have to think about it.
    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Store {
    /// The default backing-file name inside a given config/home directory.
    pub fn default_path(home: &Path) -> PathBuf {
        home.join("agos-proxy.db")
    }

    /// Open the store at `path`, creating the file and schema if needed.
    pub fn open(path: PathBuf) -> Result<Self> {
        let conn = Connection::open(&path).context("opening the database file")?;
        conn.execute_batch(schema::SCHEMA)
            .context("applying the database schema")?;
        schema::migrate_columns(&conn).context("migrating the database schema")?;
        let store = Store {
            conn: Mutex::new(conn),
            path,
            master_key: Mutex::new(None),
        };
        store.load_or_generate_master_key()?;
        Ok(store)
    }

    /// Open an in-memory store, useful for tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(schema::SCHEMA)?;
        schema::migrate_columns(&conn)?;
        let store = Store {
            conn: Mutex::new(conn),
            path: PathBuf::from(":memory:"),
            master_key: Mutex::new(None),
        };
        store.load_or_generate_master_key()?;
        Ok(store)
    }

    /// Load the master key from the `meta` table, or generate and persist one.
    fn load_or_generate_master_key(&self) -> Result<()> {
        let mut cached = self
            .master_key
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cached.is_some() {
            return Ok(());
        }
        let conn = self.conn();
        let result: Option<Vec<u8>> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'master_key'",
                [],
                |row| row.get(0),
            )
            .optional()
            .context("reading master key from meta")?;
        let key = match result {
            Some(bytes) => MasterKey::from_bytes(&bytes)?,
            None => {
                let key = MasterKey::generate()?;
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES ('master_key', ?1)",
                    [key.as_bytes().to_vec()],
                )
                .context("persisting master key")?;
                key
            }
        };
        *cached = Some(key);
        Ok(())
    }

    /// Get the master key, loading it if needed.
    fn master_key(&self) -> MasterKey {
        if let Err(_e) = self.load_or_generate_master_key() {
            // If loading fails, generate a fresh in-memory key. This avoids
            // crashing the process while still allowing crypto operations to
            // proceed (requests will fail with decryption errors for data
            // encrypted under the old key, which is the expected behavior).
            return MasterKey::generate().expect("master key generation must succeed");
        }
        self.master_key
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("master key must be cached after successful load")
    }

    /// Base location of the backing database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    // --- profiles -----------------------------------------------------------

    /// Create a profile, returning it with a freshly generated token.
    pub fn create_profile(
        &self,
        name: &str,
        description: Option<&str>,
        password_hash: Option<&str>,
    ) -> Result<Profile> {
        let now = now_millis();
        let id = fresh_token()?;
        self.conn()
            .execute(
                "INSERT INTO profiles (id, name, description, password_hash, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                (&id, name, description, password_hash, now, now),
            )
            .context("inserting the profile")?;
        Ok(Profile {
            id,
            name: name.to_string(),
            description: description.map(|d| d.to_string()),
            password_hash: password_hash.map(|p| p.to_string()),
            created_at: now,
            updated_at: now,
            rpm_limit: 0,
        })
    }

    /// All profiles, ordered by name.
    pub fn list_profiles(&self) -> Result<Vec<Profile>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, description, password_hash, created_at, updated_at, rpm_limit
                 FROM profiles ORDER BY name",
        )?;
        let rows = stmt.query_map((), |row| {
            Ok(Profile {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                password_hash: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
                rpm_limit: row.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for item in rows {
            out.push(item?);
        }
        Ok(out)
    }

    /// Look up a profile by its id/token.
    pub fn get_profile_by_id(&self, id: &str) -> Result<Option<Profile>> {
        self.conn()
            .query_row(
                "SELECT id, name, description, password_hash, created_at, updated_at, rpm_limit\n             FROM profiles WHERE id = ?1",
                (id,),
                |row| {
                    Ok(Profile {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        description: row.get(2)?,
                        password_hash: row.get(3)?,
                        created_at: row.get(4)?,
                        updated_at: row.get(5)?,
                        rpm_limit: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.into())
    }

    /// Look up a profile by its display name.
    pub fn get_profile_by_name(&self, name: &str) -> Result<Option<Profile>> {
        self.conn()
            .query_row(
                "SELECT id, name, description, password_hash, created_at, updated_at, rpm_limit
             FROM profiles WHERE name = ?1",
                (name,),
                |row| {
                    Ok(Profile {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        description: row.get(2)?,
                        password_hash: row.get(3)?,
                        created_at: row.get(4)?,
                        updated_at: row.get(5)?,
                        rpm_limit: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.into())
    }

    /// Delete a profile and everything under it (cascades).
    pub fn delete_profile(&self, id: &str) -> Result<()> {
        self.conn()
            .execute("DELETE FROM profiles WHERE id = ?1", (id,))
            .context("deleting the profile")?;
        Ok(())
    }

    /// Generate a fresh API token for a profile, replacing the old one.
    pub fn rotate_profile_token(&self, id: &str) -> Result<String> {
        let new_token = fresh_token()?;
        let now = now_millis();
        let changed = self
            .conn()
            .execute(
                "UPDATE profiles SET id = ?1, updated_at = ?2 WHERE id = ?3",
                (&new_token, now, id),
            )
            .context("rotating the profile token")?;
        if changed == 0 {
            bail!("no profile matches id {id:?}");
        }
        Ok(new_token)
    }

    /// Set the requests-per-minute ceiling for a profile (0 = unlimited).
    pub fn set_profile_rpm_limit(&self, id: &str, rpm_limit: i64) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE profiles SET rpm_limit = ?1, updated_at = ?2 WHERE id = ?3",
                (rpm_limit, now_millis(), id),
            )
            .context("updating the profile rpm limit")?;
        if changed == 0 {
            bail!("no profile matches id {id:?}");
        }
        Ok(())
    }

    /// Rename a profile.
    pub fn rename_profile(&self, id: &str, new_name: &str) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE profiles SET name = ?1, updated_at = ?2 WHERE id = ?3",
                (new_name, now_millis(), id),
            )
            .context("renaming the profile")?;
        if changed == 0 {
            bail!("no profile matches id {id:?}");
        }
        Ok(())
    }

    /// Set (or clear) a profile's free-text description.
    pub fn set_profile_description(&self, id: &str, description: Option<&str>) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE profiles SET description = ?1, updated_at = ?2 WHERE id = ?3",
                (description, now_millis(), id),
            )
            .context("updating the profile description")?;
        if changed == 0 {
            bail!("no profile matches id {id:?}");
        }
        Ok(())
    }

    /// Set a profile's Argon2 password hash.
    pub fn set_profile_password(&self, id: &str, hash: &str) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE profiles SET password_hash = ?1, updated_at = ?2 WHERE id = ?3",
                (hash, now_millis(), id),
            )
            .context("setting the profile password")?;
        if changed == 0 {
            bail!("no profile matches id {id:?}");
        }
        Ok(())
    }

    /// Remove a profile's password so future changes are no longer gated.
    pub fn clear_profile_password(&self, id: &str) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE profiles SET password_hash = NULL, updated_at = ?1 WHERE id = ?2",
                (now_millis(), id),
            )
            .context("clearing the profile password")?;
        if changed == 0 {
            bail!("no profile matches id {id:?}");
        }
        Ok(())
    }

    // --- providers ----------------------------------------------------------

    /// Add a provider under a profile.
    pub fn create_provider(&self, profile_id: &str, spec: NewProvider) -> Result<Provider> {
        let key = self.master_key();
        let encrypted_token = crate::crypto::encrypt(&key, &spec.auth_token)?;
        let _ = self.conn()
            .execute(
                "INSERT INTO providers (profile_id, name, description, base_url, auth_token, kind, extra_headers)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                (
                    profile_id,
                    spec.name.as_str(),
                    spec.description.as_deref(),
                    spec.base_url.as_str(),
                    encrypted_token,
                    provider_kind_tag(spec.kind),
                    serde_json::to_string(&spec.extra_headers).expect("BTreeMap<String,String> always serializable"),
                ),
            )
            .context("inserting the provider")?;
        Ok(Provider {
            id: self.conn().last_insert_rowid(),
            profile_id: profile_id.to_string(),
            name: spec.name.clone(),
            description: spec.description.clone(),
            base_url: spec.base_url.clone(),
            auth_token: spec.auth_token.clone(),
            kind: spec.kind,
            extra_headers: spec.extra_headers.clone(),
        })
    }

    /// All providers belonging to a profile.
    pub fn list_providers(&self, profile_id: &str) -> Result<Vec<Provider>> {
        let conn = self.conn();
        let key = self.master_key().clone();
        let mut stmt = conn.prepare(
            "SELECT id, profile_id, name, description, base_url, auth_token, kind, extra_headers
             FROM providers WHERE profile_id = ?1 ORDER BY name",
        )?;
        let rows = stmt.query_map((profile_id,), |row| {
            Ok(RawProvider {
                id: row.get(0)?,
                profile_id: row.get(1)?,
                name: row.get(2)?,
                description: row.get(3)?,
                base_url: row.get(4)?,
                enc_token: row.get(5)?,
                kind_tag: row.get(6)?,
                extra_json: row.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for item in rows {
            let raw = item?;
            let auth_token = crate::crypto::decrypt(&key, &raw.enc_token)?;
            out.push(Provider {
                id: raw.id,
                profile_id: raw.profile_id,
                name: raw.name,
                description: raw.description,
                base_url: raw.base_url,
                auth_token,
                kind: provider_kind_from_tag(&raw.kind_tag)
                    .expect("invalid provider kind in store"),
                extra_headers: serde_json::from_str::<std::collections::BTreeMap<String, String>>(
                    &raw.extra_json,
                )
                .expect("invalid provider headers in store"),
            });
        }
        Ok(out)
    }

    /// A single provider by id.
    pub fn get_provider(&self, id: i64) -> Result<Option<Provider>> {
        let key = self.master_key().clone();
        let raw: Option<RawProvider> = self.conn().query_row(
            "SELECT id, profile_id, name, description, base_url, auth_token, kind, extra_headers
             FROM providers WHERE id = ?1",
            (id,),
            |row| {
                Ok(RawProvider {
                    id: row.get(0)?,
                    profile_id: row.get(1)?,
                    name: row.get(2)?,
                    description: row.get(3)?,
                    base_url: row.get(4)?,
                    enc_token: row.get(5)?,
                    kind_tag: row.get(6)?,
                    extra_json: row.get(7)?,
                })
            },
        ).optional()?;
        match raw {
            Some(raw) => {
                let auth_token = crate::crypto::decrypt(&key, &raw.enc_token)?;
                Ok(Some(Provider {
                    id: raw.id,
                    profile_id: raw.profile_id,
                    name: raw.name,
                    description: raw.description,
                    base_url: raw.base_url,
                    auth_token,
                    kind: provider_kind_from_tag(&raw.kind_tag)
                        .expect("invalid provider kind in store"),
                    extra_headers:
                        serde_json::from_str::<std::collections::BTreeMap<String, String>>(
                            &raw.extra_json,
                        )
                        .expect("invalid provider headers in store"),
                }))
            }
            None => Ok(None),
        }
    }

    /// Remove a provider.
    pub fn delete_provider(&self, id: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM providers WHERE id = ?1", (id,))
            .context("deleting the provider")?;
        Ok(())
    }

    /// Update a provider's settings (name, description, base URL, token, kind, headers).
    pub fn update_provider(&self, id: i64, spec: NewProvider) -> Result<()> {
        let key = self.master_key();
        let encrypted_token = crate::crypto::encrypt(&key, &spec.auth_token)?;
        let changed = self
            .conn()
            .execute(
                "UPDATE providers SET name = ?1, description = ?2, base_url = ?3, auth_token = ?4, kind = ?5, extra_headers = ?6 WHERE id = ?7",
                (
                    spec.name.as_str(),
                    spec.description.as_deref(),
                    spec.base_url.as_str(),
                    encrypted_token,
                    provider_kind_tag(spec.kind),
                    serde_json::to_string(&spec.extra_headers).expect("BTreeMap<String,String> always serializable"),
                    id,
                ),
            )
            .context("updating the provider")?;
        if changed == 0 {
            bail!("no provider matches id {id}");
        }
        Ok(())
    }

    // --- proxies and routes --------------------------------------------------

    /// Create a proxy under a profile.
    pub fn create_proxy(
        &self,
        profile_id: &str,
        name: &str,
        description: Option<&str>,
    ) -> Result<Proxy> {
        let _ = self
            .conn()
            .execute(
                "INSERT INTO proxies (profile_id, name, description) VALUES (?1, ?2, ?3)",
                (profile_id, name, description),
            )
            .context("inserting the proxy")?;
        Ok(Proxy {
            id: self.conn().last_insert_rowid(),
            profile_id: profile_id.to_string(),
            name: name.to_string(),
            description: description.map(|d| d.to_string()),
        })
    }

    /// All proxies under a profile.
    pub fn list_proxies(&self, profile_id: &str) -> Result<Vec<Proxy>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, profile_id, name, description FROM proxies WHERE profile_id = ?1 ORDER BY name",
        )?;
        let rows = stmt.query_map((profile_id,), |row| {
            Ok(Proxy {
                id: row.get(0)?,
                profile_id: row.get(1)?,
                name: row.get(2)?,
                description: row.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for item in rows {
            out.push(item?);
        }
        Ok(out)
    }

    /// Find a proxy by id.
    pub fn get_proxy(&self, id: i64) -> Result<Option<Proxy>> {
        self.conn()
            .query_row(
                "SELECT id, profile_id, name, description FROM proxies WHERE id = ?1",
                (id,),
                |row| {
                    Ok(Proxy {
                        id: row.get(0)?,
                        profile_id: row.get(1)?,
                        name: row.get(2)?,
                        description: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.into())
    }

    /// Find a proxy within a profile by name.
    pub fn get_proxy_named(&self, profile_id: &str, name: &str) -> Result<Option<Proxy>> {
        self.conn()
            .query_row(
                "SELECT id, profile_id, name, description FROM proxies WHERE profile_id = ?1 AND name = ?2",
                (profile_id, name),
                |row| {
                    Ok(Proxy {
                        id: row.get(0)?,
                        profile_id: row.get(1)?,
                        name: row.get(2)?,
                        description: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.into())
    }

    /// Remove a proxy and all of its routes.
    pub fn delete_proxy(&self, id: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM proxies WHERE id = ?1", (id,))
            .context("deleting the proxy")?;
        Ok(())
    }

    /// Update a proxy's name and description.
    pub fn update_proxy(&self, id: i64, name: &str, description: Option<&str>) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE proxies SET name = ?1, description = ?2 WHERE id = ?3",
                (name, description, id),
            )
            .context("updating the proxy")?;
        if changed == 0 {
            bail!("no proxy matches id {id}");
        }
        Ok(())
    }

    /// Create a route under a proxy.
    pub fn create_route(
        &self,
        proxy_id: i64,
        name: &str,
        description: Option<&str>,
        strategy: RoutingStrategy,
    ) -> Result<Route> {
        let _ = self.conn()
            .execute(
                "INSERT INTO routes (proxy_id, name, description, strategy) VALUES (?1, ?2, ?3, ?4)",
                (proxy_id, name, description, strategy_tag(strategy)),
            )
            .context("inserting the route")?;
        Ok(Route {
            id: self.conn().last_insert_rowid(),
            proxy_id,
            name: name.to_string(),
            description: description.map(|d| d.to_string()),
            strategy,
        })
    }

    /// Remove a route and its model chain.
    pub fn delete_route(&self, id: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM routes WHERE id = ?1", (id,))
            .context("deleting the route")?;
        Ok(())
    }

    /// Update a route's name, description and routing strategy.
    pub fn update_route(
        &self,
        id: i64,
        name: &str,
        description: Option<&str>,
        strategy: RoutingStrategy,
    ) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE routes SET name = ?1, description = ?2, strategy = ?3 WHERE id = ?4",
                (name, description, strategy_tag(strategy), id),
            )
            .context("updating the route")?;
        if changed == 0 {
            bail!("no route matches id {id}");
        }
        Ok(())
    }

    /// All routes under a proxy.
    pub fn list_routes(&self, proxy_id: i64) -> Result<Vec<Route>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, proxy_id, name, description, strategy FROM routes WHERE proxy_id = ?1 ORDER BY name",
        )?;
        let rows = stmt.query_map((proxy_id,), |row| {
            let strat_tag: String = row.get(4)?;
            Ok(Route {
                id: row.get(0)?,
                proxy_id: row.get(1)?,
                name: row.get(2)?,
                description: row.get(3)?,
                strategy: strategy_from_tag(&strat_tag).expect("invalid strategy in store"),
            })
        })?;
        let mut out = Vec::new();
        for item in rows {
            out.push(item?);
        }
        Ok(out)
    }

    /// Find a route within a proxy by name.
    pub fn get_route_named(&self, proxy_id: i64, name: &str) -> Result<Option<Route>> {
        self.conn()
            .query_row(
                "SELECT id, proxy_id, name, description, strategy FROM routes WHERE proxy_id = ?1 AND name = ?2",
                (proxy_id, name),
                |row| {
                    let strat_tag: String = row.get(4)?;
                    Ok(Route {
                        id: row.get(0)?,
                        proxy_id: row.get(1)?,
                        name: row.get(2)?,
                        description: row.get(3)?,
                        strategy: strategy_from_tag(&strat_tag).expect("invalid strategy in store"),
                    })
                },
            )
            .optional()
            .map_err(|e| e.into())
    }

    /// Append a model to a route's fallback chain.
    pub fn add_route_entry(
        &self,
        route_id: i64,
        provider_id: i64,
        model_id: &str,
        priority: i32,
        weight: f64,
        capabilities: RouteCapabilities,
    ) -> Result<RouteEntry> {
        let _ = self.conn()
            .execute(
                "INSERT INTO route_entries (route_id, provider_id, model_id, priority, weight, status, capabilities)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                (
                    route_id,
                    provider_id,
                    model_id,
                    priority,
                    weight,
                    status_tag(ModelStatus::Healthy),
                    serde_json::to_string(&capabilities).expect("RouteCapabilities always serializable"),
                ),
            )
            .context("inserting the route model")?;
        Ok(RouteEntry {
            id: self.conn().last_insert_rowid(),
            route_id,
            provider_id,
            model_id: model_id.to_string(),
            priority,
            weight,
            status: ModelStatus::Healthy,
            capabilities,
        })
    }

    /// All model entries in a route's chain, ordered by priority.
    pub fn route_entries(&self, route_id: i64) -> Result<Vec<RouteEntry>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, route_id, provider_id, model_id, priority, weight, status, capabilities
             FROM route_entries WHERE route_id = ?1 ORDER BY priority",
        )?;
        let rows = stmt.query_map((route_id,), |row| {
            let status_tag_owned: String = row.get(6)?;
            let caps_json: String = row.get(7)?;
            Ok(RouteEntry {
                id: row.get(0)?,
                route_id: row.get(1)?,
                provider_id: row.get(2)?,
                model_id: row.get(3)?,
                priority: row.get(4)?,
                weight: row.get(5)?,
                status: status_from_tag(&status_tag_owned).expect("invalid status in store"),
                capabilities: serde_json::from_str::<RouteCapabilities>(&caps_json)
                    .expect("invalid capabilities in store"),
            })
        })?;
        let mut out = Vec::new();
        for item in rows {
            out.push(item?);
        }
        Ok(out)
    }

    /// Update the health state of a route entry.
    pub fn set_route_entry_status(&self, entry_id: i64, status: ModelStatus) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE route_entries SET status = ?1 WHERE id = ?2",
                (status_tag(status), entry_id),
            )
            .context("updating the route model status")?;
        Ok(())
    }

    /// Remove a model from a route's chain.
    pub fn delete_route_entry(&self, id: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM route_entries WHERE id = ?1", (id,))
            .context("deleting the route model")?;
        Ok(())
    }

    /// Re-point a route entry to a different provider/model and tune its weight
    /// and capabilities, keeping its position in the chain.
    pub fn update_route_entry(
        &self,
        id: i64,
        model_id: &str,
        provider_id: i64,
        weight: f64,
        capabilities: RouteCapabilities,
    ) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE route_entries SET model_id = ?1, provider_id = ?2, weight = ?3, capabilities = ?4 WHERE id = ?5",
                (
                    model_id,
                    provider_id,
                    weight,
                    serde_json::to_string(&capabilities).expect("RouteCapabilities always serializable"),
                    id,
                ),
            )
            .context("updating the route model")?;
        if changed == 0 {
            bail!("no route model matches id {id}");
        }
        Ok(())
    }

    /// Move a route entry to a new position in the fallback chain.
    pub fn set_route_entry_priority(&self, id: i64, priority: i32) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE route_entries SET priority = ?1 WHERE id = ?2",
                (priority, id),
            )
            .context("reordering the route model")?;
        Ok(())
    }

    /// All entries that are not Healthy and not Disabled — these are the ones
    /// the health probe should check.
    pub fn entries_needing_probe(&self) -> Result<Vec<(RouteEntry, Provider)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT e.id, e.route_id, e.provider_id, e.model_id, e.priority,
                    e.weight, e.status, e.capabilities,
                    p.id, p.profile_id, p.name, p.description, p.base_url,
                    p.auth_token, p.kind, p.extra_headers
             FROM route_entries e
             JOIN providers p ON p.id = e.provider_id
             WHERE e.status != ?1 AND e.status != ?2",
        )?;
        let rows = stmt.query_map(
            [
                status_tag(ModelStatus::Healthy),
                status_tag(ModelStatus::Disabled),
            ],
            |row| {
                let status_tag_owned: String = row.get(6)?;
                let caps_json: String = row.get(7)?;
                let kind_tag: String = row.get(14)?;
                let extra_json: String = row.get(15)?;
                Ok((
                    RouteEntry {
                        id: row.get(0)?,
                        route_id: row.get(1)?,
                        provider_id: row.get(2)?,
                        model_id: row.get(3)?,
                        priority: row.get(4)?,
                        weight: row.get(5)?,
                        status: status_from_tag(&status_tag_owned)
                            .expect("invalid status in store"),
                        capabilities: serde_json::from_str::<RouteCapabilities>(&caps_json)
                            .expect("invalid capabilities in store"),
                    },
                    Provider {
                        id: row.get(8)?,
                        profile_id: row.get(9)?,
                        name: row.get(10)?,
                        description: row.get(11)?,
                        base_url: row.get(12)?,
                        auth_token: row.get(13)?,
                        kind: provider_kind_from_tag(&kind_tag).expect("invalid kind in store"),
                        extra_headers: serde_json::from_str(&extra_json)
                            .expect("invalid headers in store"),
                    },
                ))
            },
        )?;
        let mut out = Vec::new();
        for item in rows {
            out.push(item?);
        }
        Ok(out)
    }

    // ---- usage logging -------------------------------------------------

    /// Append one completed request to the usage log.
    pub fn record_usage(&self, rec: NewUsage) -> Result<UsageRecord> {
        let now = now_millis();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO usage_log
                (profile_id, route_entry_id, model_id, streamed, success,
                 status_code, error_message, latency_ms, prompt_tokens,
                 completion_tokens, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                rec.profile_id,
                rec.route_entry_id,
                rec.model_id,
                rec.streamed as i64,
                rec.success as i64,
                rec.status_code,
                rec.error_message,
                rec.latency_ms,
                rec.prompt_tokens,
                rec.completion_tokens,
                now,
            ],
        )?;
        let id = conn.last_insert_rowid();
        Ok(UsageRecord {
            id,
            profile_id: rec.profile_id,
            route_entry_id: rec.route_entry_id,
            model_id: rec.model_id,
            streamed: rec.streamed,
            success: rec.success,
            status_code: rec.status_code,
            error_message: rec.error_message,
            latency_ms: rec.latency_ms,
            prompt_tokens: rec.prompt_tokens,
            completion_tokens: rec.completion_tokens,
            created_at: now,
        })
    }

    /// The most recent usage records for a profile, newest first.
    pub fn list_usage(&self, profile_id: &str, limit: u32) -> Result<Vec<UsageRecord>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, profile_id, route_entry_id, model_id, streamed, success,
                    status_code, error_message, latency_ms, prompt_tokens,
                    completion_tokens, created_at
             FROM usage_log
             WHERE profile_id = ?1
             ORDER BY created_at DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map([profile_id, &limit.to_string()], |row| {
            Ok(UsageRecord {
                id: row.get(0)?,
                profile_id: row.get(1)?,
                route_entry_id: row.get(2)?,
                model_id: row.get(3)?,
                streamed: row.get::<_, i64>(4)? != 0,
                success: row.get::<_, i64>(5)? != 0,
                status_code: row.get(6)?,
                error_message: row.get(7)?,
                latency_ms: row.get(8)?,
                prompt_tokens: row.get(9)?,
                completion_tokens: row.get(10)?,
                created_at: row.get(11)?,
            })
        })?;
        let mut out = Vec::new();
        for item in rows {
            out.push(item?);
        }
        Ok(out)
    }

    /// Per-model aggregates (calls, failures, latency, tokens) for a profile.
    pub fn usage_stats(&self, profile_id: &str) -> Result<Vec<UsageStats>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT model_id,
                    COUNT(*) AS calls,
                    SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END) AS failures,
                    AVG(latency_ms),
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0)
             FROM usage_log
             WHERE profile_id = ?1
             GROUP BY model_id
             ORDER BY calls DESC",
        )?;
        let rows = stmt.query_map([profile_id], |row| {
            Ok(UsageStats {
                model_id: row.get(0)?,
                calls: row.get(1)?,
                failures: row.get(2)?,
                avg_latency_ms: row.get(3)?,
                prompt_tokens: row.get(4)?,
                completion_tokens: row.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for item in rows {
            out.push(item?);
        }
        Ok(out)
    }

    /// Remove usage records older than `keep_days` days. Returns the number of rows deleted.
    /// Called periodically to prevent unbounded database growth.
    pub fn prune_usage_log(&self, keep_days: u32) -> Result<usize> {
        let cutoff = chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::days(keep_days as i64))
            .expect("invalid cutoff date")
            .timestamp_millis();
        let deleted = self
            .conn()
            .execute("DELETE FROM usage_log WHERE created_at < ?1", (cutoff,))?;
        Ok(deleted)
    }
}

/// Exercise the full CRUD round-trip against an in-memory store.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ProviderKind, RoutingStrategy};

    #[test]
    fn rpm_limit_roundtrip_and_unknown_profile() -> Result<()> {
        let store = Store::open_in_memory()?;

        // Fresh profiles start unlimited.
        let profile = store.create_profile("lim", None, None)?;
        assert_eq!(profile.rpm_limit, 0);
        assert_eq!(store.get_profile_by_name("lim")?.unwrap().rpm_limit, 0);

        // Setting a limit is visible through every read path.
        store.set_profile_rpm_limit(profile.id.as_str(), 120)?;
        assert_eq!(store.get_profile_by_name("lim")?.unwrap().rpm_limit, 120);
        assert_eq!(
            store
                .get_profile_by_id(profile.id.as_str())?
                .unwrap()
                .rpm_limit,
            120
        );
        assert_eq!(store.list_profiles()?[0].rpm_limit, 120);

        // Back to unlimited.
        store.set_profile_rpm_limit(profile.id.as_str(), 0)?;
        assert_eq!(store.get_profile_by_name("lim")?.unwrap().rpm_limit, 0);

        // Unknown ids are rejected rather than silently ignored.
        assert!(store.set_profile_rpm_limit("no-such-id", 10).is_err());
        Ok(())
    }

    #[test]
    fn profile_and_provider_roundtrip() -> Result<()> {
        let store = Store::open_in_memory()?;

        let profile = store.create_profile("coder1", Some("primary"), None)?;
        assert!(store.get_profile_by_id(profile.id.as_str())?.is_some());
        assert!(store.get_profile_by_name("coder1")?.is_some());
        assert_eq!(store.list_profiles()?.len(), 1);

        let mut headers = std::collections::BTreeMap::new();
        headers.insert("X-Custom".to_string(), "yes".to_string());
        let provider = store.create_provider(
            profile.id.as_str(),
            NewProvider {
                name: "Deepseek".to_string(),
                description: None,
                base_url: "https://api.deepseek.com".to_string(),
                auth_token: "sk-secret".to_string(),
                kind: ProviderKind::OpenAICompatible,
                extra_headers: headers,
            },
        )?;
        let listed = store.list_providers(profile.id.as_str())?;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].auth_token, "sk-secret");

        store.delete_provider(provider.id)?;
        assert_eq!(store.list_providers(profile.id.as_str())?.len(), 0);

        store.delete_profile(profile.id.as_str())?;
        assert_eq!(store.list_profiles()?.len(), 0);
        Ok(())
    }

    #[test]
    fn crud_update_delete_end_to_end() -> Result<()> {
        let store = Store::open_in_memory()?;
        let profile = store.create_profile("acct", Some("desc"), None)?;

        // Profile editing.
        store.rename_profile(profile.id.as_str(), "acct2")?;
        assert_eq!(store.get_profile_by_name("acct2")?.unwrap().name, "acct2");
        store.set_profile_description(profile.id.as_str(), Some("new desc"))?;
        assert_eq!(
            store.get_profile_by_name("acct2")?.unwrap().description,
            Some("new desc".to_string())
        );
        store.set_profile_password(profile.id.as_str(), "argon2:abc")?;
        assert!(store
            .get_profile_by_name("acct2")?
            .unwrap()
            .password_hash
            .is_some());
        store.clear_profile_password(profile.id.as_str())?;
        assert!(!store
            .get_profile_by_name("acct2")?
            .unwrap()
            .password_hash
            .is_some());

        // Provider edit (token re-encrypted and readable).
        let provider = store.create_provider(
            profile.id.as_str(),
            NewProvider {
                name: "p1".to_string(),
                description: None,
                base_url: "https://a".to_string(),
                auth_token: "t1".to_string(),
                kind: ProviderKind::OpenAICompatible,
                extra_headers: std::collections::BTreeMap::new(),
            },
        )?;
        store.update_provider(
            provider.id,
            NewProvider {
                name: "p2".to_string(),
                description: Some("d".to_string()),
                base_url: "https://b".to_string(),
                auth_token: "t2".to_string(),
                kind: ProviderKind::Anthropic,
                extra_headers: std::collections::BTreeMap::new(),
            },
        )?;
        let updated = store.get_provider(provider.id)?.unwrap();
        assert_eq!(updated.name, "p2");
        assert_eq!(updated.base_url, "https://b");
        assert_eq!(updated.auth_token, "t2");
        assert_eq!(updated.kind, ProviderKind::Anthropic);

        // Proxy edit + delete.
        let proxy = store.create_proxy(profile.id.as_str(), "main", Some("x"))?;
        store.update_proxy(proxy.id, "main2", None)?;
        assert!(store
            .get_proxy_named(profile.id.as_str(), "main2")?
            .is_some());

        // Route edit + strategy change + entry reorder/update/delete.
        let route = store.create_route(proxy.id, "r", None, RoutingStrategy::Priority)?;
        store.update_route(route.id, "r2", Some("desc"), RoutingStrategy::Weighted)?;
        let route2 = store.get_route_named(proxy.id, "r2")?.unwrap();
        assert_eq!(route2.strategy, RoutingStrategy::Weighted);

        let e1 = store.add_route_entry(
            route.id,
            provider.id,
            "m1",
            1,
            1.0,
            RouteCapabilities::default(),
        )?;
        let e2 = store.add_route_entry(
            route.id,
            provider.id,
            "m2",
            2,
            1.0,
            RouteCapabilities::default(),
        )?;
        store.update_route_entry(e1.id, "m1b", provider.id, 2.0, RouteCapabilities::default())?;
        store.set_route_entry_priority(e1.id, 2)?;
        store.set_route_entry_priority(e2.id, 1)?;
        let entries = store.route_entries(route.id)?;
        assert_eq!(entries[0].model_id, "m2"); // e2 moved to the front
        assert_eq!(entries[1].model_id, "m1b");
        assert_eq!(entries[1].weight, 2.0);

        store.delete_route_entry(e2.id)?;
        assert_eq!(store.route_entries(route.id)?.len(), 1);

        store.delete_route(route.id)?;
        assert_eq!(store.list_routes(proxy.id)?.len(), 0);

        store.delete_proxy(proxy.id)?;
        assert_eq!(store.list_proxies(profile.id.as_str())?.len(), 0);

        store.delete_provider(provider.id)?;
        store.delete_profile(profile.id.as_str())?;
        assert_eq!(store.list_profiles()?.len(), 0);
        Ok(())
    }

    #[test]
    fn route_chain_resolves_in_priority_order() -> Result<()> {
        let store = Store::open_in_memory()?;
        let profile = store.create_profile("coder1", None, None)?;

        let deepseek = store.create_provider(
            profile.id.as_str(),
            NewProvider {
                name: "Deepseek".to_string(),
                description: None,
                base_url: "https://api.deepseek.com".to_string(),
                auth_token: "a".to_string(),
                kind: ProviderKind::OpenAICompatible,
                extra_headers: std::collections::BTreeMap::new(),
            },
        )?;
        let openrouter = store.create_provider(
            profile.id.as_str(),
            NewProvider {
                name: "OpenRouter".to_string(),
                description: None,
                base_url: "https://openrouter.ai".to_string(),
                auth_token: "b".to_string(),
                kind: ProviderKind::OpenAICompatible,
                extra_headers: std::collections::BTreeMap::new(),
            },
        )?;

        let proxy = store.create_proxy(profile.id.as_str(), "Programmer", None)?;
        let route = store.create_route(
            proxy.id,
            "php-developer-3.5-flash",
            None,
            RoutingStrategy::Priority,
        )?;

        let second = store.add_route_entry(
            route.id,
            openrouter.id,
            "glm-5.3-flash",
            2,
            1.0,
            RouteCapabilities::default(),
        )?;
        let first = store.add_route_entry(
            route.id,
            deepseek.id,
            "deepseek-v4-flash",
            1,
            1.0,
            RouteCapabilities::default(),
        )?;

        let chain = store.route_entries(route.id)?;
        // Insertion order was reversed, but the chain must surface priority order.
        assert_eq!(chain[0].provider_id, deepseek.id);
        assert_eq!(chain[0].id, first.id);
        assert_eq!(chain[1].provider_id, openrouter.id);
        assert_eq!(chain[1].id, second.id);

        store.set_route_entry_status(first.id, ModelStatus::Unhealthy)?;
        let updated = store.route_entries(route.id)?;
        assert_eq!(updated[0].status, ModelStatus::Unhealthy);
        Ok(())
    }
}

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
    KeyStats, MaskingServer, ModelStatus, Profile, PromptCachePolicy, Provider, ProviderKind,
    Proxy, Route, RouteCapabilities, RouteEntry, RoutingStrategy, UsageRecord, UsageStats,
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
    masking_server_id: Option<i64>,
    shared: bool,
}

/// Intermediate masking-server row, used to defer secret decryption out of the
/// rusqlite closure.
struct RawMaskingServer {
    id: i64,
    profile_id: String,
    name: String,
    kind: String,
    endpoint_url: String,
    enc_secret: Vec<u8>,
    max_body_bytes: i64,
    expected_egress_ip: Option<String>,
    last_verified_ip: Option<String>,
    last_verified_asn: Option<String>,
    last_verified_country: Option<String>,
    last_verified_at: Option<i64>,
    shared: bool,
}

/// The [`MaskingServer`] column list, optionally qualified with a table alias
/// (e.g. `"m."`). Kept in one place so every query reads the same shape and the
/// row indices handed to [`raw_mask_from_row`] stay in sync.
pub(crate) fn mask_columns(prefix: &str) -> String {
    const COLS: [&str; 13] = [
        "id",
        "profile_id",
        "name",
        "kind",
        "endpoint_url",
        "secret",
        "max_body_bytes",
        "expected_egress_ip",
        "last_verified_ip",
        "last_verified_asn",
        "last_verified_country",
        "last_verified_at",
        "shared",
    ];
    COLS.iter()
        .map(|c| format!("{prefix}{c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Read a [`MaskingServer`] row starting at `base` (0 for a plain select, or the
/// offset of the joined mask columns). Returns `None` when the joined row has no
/// mask, so a `LEFT JOIN` can be mapped in one pass.
fn raw_mask_from_row(
    row: &rusqlite::Row<'_>,
    base: usize,
) -> rusqlite::Result<Option<RawMaskingServer>> {
    let id: Option<i64> = row.get(base)?;
    let Some(id) = id else {
        return Ok(None);
    };
    Ok(Some(RawMaskingServer {
        id,
        profile_id: row.get(base + 1)?,
        name: row.get(base + 2)?,
        kind: row.get(base + 3)?,
        endpoint_url: row.get(base + 4)?,
        enc_secret: row.get(base + 5)?,
        max_body_bytes: row.get(base + 6)?,
        expected_egress_ip: row.get(base + 7)?,
        last_verified_ip: row.get(base + 8)?,
        last_verified_asn: row.get(base + 9)?,
        last_verified_country: row.get(base + 10)?,
        last_verified_at: row.get(base + 11)?,
        shared: row.get::<_, Option<i64>>(base + 12)?.unwrap_or(0) != 0,
    }))
}

mod schema;

// --- enum <-> string mapping -------------------------------------------------
// The wire/DB representation is a short, stable tag rather than the Rust variant
// name, so shuffled enum ordering never leaks into stored data.

fn provider_kind_tag(k: ProviderKind) -> &'static str {
    match k {
        ProviderKind::OpenAI => "openai",
        ProviderKind::Anthropic => "anthropic",
        ProviderKind::Google => "google",
        ProviderKind::OpenAIResponses => "openai_responses",
        ProviderKind::Custom => "custom",
    }
}

fn provider_kind_from_tag(tag: &str) -> Result<ProviderKind> {
    match tag {
        "openai" => Ok(ProviderKind::OpenAI),
        "anthropic" => Ok(ProviderKind::Anthropic),
        "google" => Ok(ProviderKind::Google),
        "openai_responses" => Ok(ProviderKind::OpenAIResponses),
        "custom" => Ok(ProviderKind::Custom),
        _ => bail!("unknown provider kind tag {tag:?}"),
    }
}

fn strategy_tag(s: RoutingStrategy) -> &'static str {
    match s {
        RoutingStrategy::Priority => "priority",
        RoutingStrategy::RoundRobin => "round_robin",
        RoutingStrategy::Weighted => "weighted",
        RoutingStrategy::Economy => "economy",
    }
}

fn strategy_from_tag(tag: &str) -> Result<RoutingStrategy> {
    match tag {
        "priority" => Ok(RoutingStrategy::Priority),
        "round_robin" => Ok(RoutingStrategy::RoundRobin),
        "weighted" => Ok(RoutingStrategy::Weighted),
        "economy" => Ok(RoutingStrategy::Economy),
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
        "draining" => {
            tracing::warn!("encountered deprecated 'draining' status, treating as Unhealthy");
            Ok(ModelStatus::Unhealthy)
        }
        _ => {
            tracing::warn!("encountered unknown model status tag {tag:?}, treating as Unhealthy");
            Ok(ModelStatus::Unhealthy)
        }
    }
}

/// A random profile token, hex-encoded (128 bits of entropy).
fn fresh_token() -> Result<String> {
    let a = getrandom::u64()?;
    let b = getrandom::u64()?;
    Ok(std::fmt::format(format_args!("{a:x}{b:x}")))
}

/// Column list for a provider joined to its egress mask.
///
/// Fixed prefix 0..9 is the provider itself (binding at 8, sharing flag at 9);
/// index 10 onwards are the mask columns consumed by [`raw_mask_from_row`].
fn provider_columns() -> String {
    format!(
        "p.id, p.profile_id, p.name, p.description, p.base_url, p.auth_token, \
         p.kind, p.extra_headers, p.masking_server_id, p.shared, {}",
        mask_columns("m.")
    )
}

type RawProviderRow = (RawProvider, Option<RawMaskingServer>);

/// Join clause that attaches each provider's *effective* mask: the provider's own
/// binding when it has one, otherwise its profile's default, otherwise nothing.
///
/// Resolving the fallback in SQL keeps every consumer — routing, health probing,
/// CLI listings — seeing the same answer without a second lookup.
fn mask_join() -> &'static str {
    "LEFT JOIN masking_servers m
        ON m.id = COALESCE(p.masking_server_id,
                           (SELECT default_masking_server_id FROM profiles WHERE id = p.profile_id))"
}

/// Read the provider part of a row starting at `base`, plus its joined mask
/// (which occupies the nine columns that follow).
fn raw_provider_at(row: &rusqlite::Row<'_>, base: usize) -> rusqlite::Result<RawProviderRow> {
    Ok((
        RawProvider {
            id: row.get(base)?,
            profile_id: row.get(base + 1)?,
            name: row.get(base + 2)?,
            description: row.get(base + 3)?,
            base_url: row.get(base + 4)?,
            enc_token: row.get(base + 5)?,
            kind_tag: row.get(base + 6)?,
            extra_json: row.get(base + 7)?,
            masking_server_id: row.get(base + 8)?,
            shared: row.get::<_, Option<i64>>(base + 9)?.unwrap_or(0) != 0,
        },
        raw_mask_from_row(row, base + 10)?,
    ))
}

fn raw_provider_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawProviderRow> {
    raw_provider_at(row, 0)
}

/// Read one `routes` row (the nine-column list used by every route query).
fn map_route_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Route> {
    let strat_tag: String = row.get(4)?;
    Ok(Route {
        id: row.get(0)?,
        proxy_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        strategy: strategy_from_tag(&strat_tag).expect("invalid strategy in store"),
        identity: row.get(5)?,
        max_tokens: row.get::<_, Option<i64>>(6)?.unwrap_or(0).max(0) as u32,
        cache_ttl_secs: row.get::<_, Option<i64>>(7)?.unwrap_or(0).max(0),
        shared: row.get::<_, Option<i64>>(8)?.unwrap_or(0) != 0,
        prompt_cache: PromptCachePolicy::from_tag(
            &row.get::<_, Option<String>>(9)?.unwrap_or_default(),
        ),
    })
}

/// Read one `route_entries` row (the eleven-column list used by every
/// route-entry query).
fn map_route_entry_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RouteEntry> {
    let status_tag_owned: String = row.get(7)?;
    let caps_json: String = row.get(8)?;
    Ok(RouteEntry {
        id: row.get(0)?,
        route_id: row.get(1)?,
        provider_id: row.get(2)?,
        target_route_id: row.get(3)?,
        model_id: row.get(4)?,
        priority: row.get(5)?,
        weight: row.get(6)?,
        status: status_from_tag(&status_tag_owned).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "invalid status in store, defaulting to Unhealthy");
            ModelStatus::Unhealthy
        }),
        capabilities: serde_json::from_str::<RouteCapabilities>(&caps_json)
            .expect("invalid capabilities in store"),
        price_per_1m: row.get::<_, Option<f64>>(9)?.unwrap_or(0.0).max(0.0),
        cooldown_until: row.get::<_, Option<i64>>(10)?.unwrap_or(0),
    })
}

fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}

/// Heuristic blended price (USD / 1M tokens) so Economy works out of the box.
/// Cheap flash/mini/micro/nano ≈ 0.4, pro/max/ultra ≈ 6.0.
pub fn default_price_for(model_id: &str) -> f64 {
    let m = model_id.to_lowercase();
    // Cheap tier markers.
    for cheap in ["mini", "micro", "flash", "nano"] {
        if m.contains(cheap) {
            return 0.4;
        }
    }
    // Flagship markers.
    for expensive in ["pro", "max", "ultra"] {
        if m.contains(expensive) {
            return 6.0;
        }
    }
    2.0
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
    /// Egress mask to bind this provider to; `None` = use the profile default.
    pub masking_server_id: Option<i64>,
    /// Publish this provider to every profile (see [`Provider::shared`]).
    pub shared: bool,
}

/// Details needed to register a new [`MaskingServer`] (an egress hop).
pub struct NewMaskingServer {
    pub name: String,
    /// Free-form backend label, e.g. `cf_worker`, `vps`, `nginx`.
    pub kind: String,
    pub endpoint_url: String,
    /// Shared secret presented to the hop; encrypted before it reaches disk.
    pub secret: String,
    /// Body-size ceiling above which requests skip this hop (0 = no limit).
    pub max_body_bytes: i64,
    pub expected_egress_ip: Option<String>,
    /// Publish this mask to every profile (see [`MaskingServer::shared`]).
    pub shared: bool,
}

/// Details needed to append one request to the usage log.
pub struct NewUsage {
    pub profile_id: String,
    /// Stable inbound `X-Request-ID` shared by every attempt in the failover chain.
    pub request_id: Option<String>,
    pub route_entry_id: i64,
    pub model_id: String,
    pub streamed: bool,
    pub success: bool,
    pub status_code: Option<i32>,
    pub error_message: Option<String>,
    pub latency_ms: i64,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    /// Input tokens the upstream billed at its cache-read rate, when it reported
    /// them. `None` means "the upstream did not say", not "nothing was cached".
    pub cached_prompt_tokens: Option<i64>,
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
        conn.execute_batch(schema::schema_sql().as_str())
            .context("applying the database schema")?;
        schema::migrate_columns(&conn).context("migrating the database schema")?;
        schema::migrate_route_entries(&conn)
            .context("migrating the route_entries table for nested routes")?;
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
        conn.execute_batch(schema::schema_sql().as_str())?;
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
        let id = fresh_token()?;
        self.insert_profile(name, description, password_hash, &id)
    }

    /// Create a profile with an explicitly supplied token (its id / bearer).
    ///
    /// Used by scripted and container provisioning where the caller must know
    /// the API token up front (e.g. the docker-compose smoke tests). The token
    /// must be unique; passing one that already exists is an error.
    pub fn create_profile_with_token(
        &self,
        name: &str,
        description: Option<&str>,
        password_hash: Option<&str>,
        token: &str,
    ) -> Result<Profile> {
        if token.is_empty() {
            bail!("profile token must not be empty");
        }
        self.insert_profile(name, description, password_hash, token)
    }

    fn insert_profile(
        &self,
        name: &str,
        description: Option<&str>,
        password_hash: Option<&str>,
        id: &str,
    ) -> Result<Profile> {
        let now = now_millis();
        self.conn()
            .execute(
                "INSERT INTO profiles (id, name, description, password_hash, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                (id, name, description, password_hash, now, now),
            )
            .context("inserting the profile")?;
        Ok(Profile {
            id: id.to_string(),
            name: name.to_string(),
            description: description.map(|d| d.to_string()),
            password_hash: password_hash.map(|p| p.to_string()),
            created_at: now,
            updated_at: now,
            rpm_limit: 0,
            default_masking_server_id: None,
        })
    }

    /// All profiles, ordered by name.
    pub fn list_profiles(&self) -> Result<Vec<Profile>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, description, password_hash, created_at, updated_at, rpm_limit, default_masking_server_id
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
                default_masking_server_id: row.get(7)?,
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
                "SELECT id, name, description, password_hash, created_at, updated_at, rpm_limit, default_masking_server_id
             FROM profiles WHERE id = ?1",
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
                        default_masking_server_id: row.get(7)?,
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
                "SELECT id, name, description, password_hash, created_at, updated_at, rpm_limit, default_masking_server_id
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
                        default_masking_server_id: row.get(7)?,
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
        self.conn()
            .execute(
                "INSERT INTO providers (profile_id, name, description, base_url, auth_token, kind, extra_headers, masking_server_id, shared)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                (
                    profile_id,
                    spec.name.as_str(),
                    spec.description.as_deref(),
                    spec.base_url.as_str(),
                    encrypted_token,
                    provider_kind_tag(spec.kind),
                    serde_json::to_string(&spec.extra_headers).expect("BTreeMap<String,String> always serializable"),
                    spec.masking_server_id,
                    spec.shared as i64,
                ),
            )
            .context("inserting the provider")?;
        let id = self.conn().last_insert_rowid();
        // Re-read so the returned value carries the resolved mask, exactly as
        // every other read path would produce it.
        self.get_provider(id)?
            .with_context(|| format!("provider {id} vanished right after insert"))
    }

    /// Decrypt a raw mask row into its domain form.
    fn build_mask(&self, raw: RawMaskingServer) -> Result<MaskingServer> {
        let key = self.master_key();
        Ok(MaskingServer {
            id: raw.id,
            profile_id: raw.profile_id,
            name: raw.name,
            kind: raw.kind,
            endpoint_url: raw.endpoint_url,
            secret: crate::crypto::decrypt(&key, &raw.enc_secret)?,
            max_body_bytes: raw.max_body_bytes.max(0),
            expected_egress_ip: raw.expected_egress_ip,
            last_verified_ip: raw.last_verified_ip,
            last_verified_asn: raw.last_verified_asn,
            last_verified_country: raw.last_verified_country,
            last_verified_at: raw.last_verified_at,
            shared: raw.shared,
        })
    }

    /// Decrypt a raw provider row and attach its egress mask, if one is bound.
    fn build_provider(&self, raw: RawProvider, mask: Option<RawMaskingServer>) -> Result<Provider> {
        let key = self.master_key();
        let auth_token = crate::crypto::decrypt(&key, &raw.enc_token)?;
        let masking_server = match mask {
            Some(m) => Some(self.build_mask(m)?),
            None => None,
        };
        Ok(Provider {
            id: raw.id,
            profile_id: raw.profile_id,
            name: raw.name,
            description: raw.description,
            base_url: raw.base_url,
            auth_token,
            kind: provider_kind_from_tag(&raw.kind_tag).expect("invalid provider kind in store"),
            extra_headers: serde_json::from_str::<std::collections::BTreeMap<String, String>>(
                &raw.extra_json,
            )
            .expect("invalid provider headers in store"),
            masking_server_id: raw.masking_server_id,
            masking_server,
            shared: raw.shared,
        })
    }

    /// Persist the latest probe result for a mask, so `mask list` and
    /// `mask audit` can show the identity without re-probing.
    pub fn record_mask_probe(
        &self,
        mask_id: i64,
        ip: &str,
        asn: Option<&str>,
        country: Option<&str>,
    ) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE masking_servers
                 SET last_verified_ip = ?1, last_verified_asn = ?2,
                     last_verified_country = ?3, last_verified_at = ?4
                 WHERE id = ?5",
                (ip, asn, country, now_millis(), mask_id),
            )
            .context("recording the mask probe")?;
        Ok(())
    }

    /// All providers belonging to a profile.
    pub fn list_providers(&self, profile_id: &str) -> Result<Vec<Provider>> {
        let raws: Vec<RawProviderRow> = {
            let conn = self.conn();
            let mut stmt = conn.prepare(&format!(
                "SELECT {} FROM providers p {}
                 WHERE p.profile_id = ?1 ORDER BY p.name",
                provider_columns(),
                mask_join()
            ))?;
            let rows = stmt.query_map((profile_id,), raw_provider_from_row)?;
            let mut out = Vec::new();
            for item in rows {
                out.push(item?);
            }
            out
        };
        raws.into_iter()
            .map(|(provider, mask)| self.build_provider(provider, mask))
            .collect()
    }

    /// Every provider a profile may route through: its own providers plus every
    /// provider another profile has shared. Owned providers come first, then
    /// shared ones ordered by name, so pickers read naturally.
    pub fn list_providers_for(&self, profile_id: &str) -> Result<Vec<Provider>> {
        let raws: Vec<RawProviderRow> = {
            let conn = self.conn();
            let mut stmt = conn.prepare(&format!(
                "SELECT {} FROM providers p {}
                 WHERE p.profile_id = ?1 OR p.shared = 1
                 ORDER BY (p.profile_id = ?1) DESC, p.name",
                provider_columns(),
                mask_join()
            ))?;
            let rows = stmt.query_map((profile_id,), raw_provider_from_row)?;
            let mut out = Vec::new();
            for item in rows {
                out.push(item?);
            }
            out
        };
        raws.into_iter()
            .map(|(provider, mask)| self.build_provider(provider, mask))
            .collect()
    }

    /// Publish (or unpublish) a provider across the instance.
    pub fn set_provider_shared(&self, provider_id: i64, shared: bool) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE providers SET shared = ?1 WHERE id = ?2",
                (shared as i64, provider_id),
            )
            .context("updating the provider's sharing flag")?;
        if changed == 0 {
            bail!("no provider matches id {provider_id}");
        }
        Ok(())
    }

    /// A single provider by id.
    pub fn get_provider(&self, id: i64) -> Result<Option<Provider>> {
        let raw: Option<RawProviderRow> = {
            let conn = self.conn();
            conn.query_row(
                &format!(
                    "SELECT {} FROM providers p {}
                     WHERE p.id = ?1",
                    provider_columns(),
                    mask_join()
                ),
                (id,),
                raw_provider_from_row,
            )
            .optional()?
        };
        match raw {
            Some((provider, mask)) => Ok(Some(self.build_provider(provider, mask)?)),
            None => Ok(None),
        }
    }

    /// Bind (or unbind, with `None`) a provider's egress mask.
    pub fn set_provider_masking(&self, provider_id: i64, mask_id: Option<i64>) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE providers SET masking_server_id = ?1 WHERE id = ?2",
                (mask_id, provider_id),
            )
            .context("binding the provider's masking server")?;
        if changed == 0 {
            bail!("no provider matches id {provider_id}");
        }
        Ok(())
    }

    // --- egress masks -------------------------------------------------------

    /// Register an egress mask under a profile.
    pub fn create_masking_server(
        &self,
        profile_id: &str,
        spec: NewMaskingServer,
    ) -> Result<MaskingServer> {
        let key = self.master_key();
        let enc_secret = crate::crypto::encrypt(&key, &spec.secret)?;
        self.conn()
            .execute(
                "INSERT INTO masking_servers
                    (profile_id, name, kind, endpoint_url, secret, max_body_bytes, expected_egress_ip, shared)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                (
                    profile_id,
                    spec.name.as_str(),
                    spec.kind.as_str(),
                    spec.endpoint_url.as_str(),
                    enc_secret,
                    spec.max_body_bytes.max(0),
                    spec.expected_egress_ip.as_deref(),
                    spec.shared as i64,
                ),
            )
            .context("inserting the masking server")?;
        let id = self.conn().last_insert_rowid();
        self.get_masking_server(id)?
            .with_context(|| format!("masking server {id} vanished right after insert"))
    }

    /// All masks belonging to a profile, ordered by name.
    pub fn list_masking_servers(&self, profile_id: &str) -> Result<Vec<MaskingServer>> {
        let raws: Vec<RawMaskingServer> = {
            let conn = self.conn();
            let mut stmt = conn.prepare(&format!(
                "SELECT {} FROM masking_servers WHERE profile_id = ?1 ORDER BY name",
                mask_columns("")
            ))?;
            let rows = stmt.query_map((profile_id,), |row| {
                raw_mask_from_row(row, 0).and_then(|m| m.ok_or(rusqlite::Error::InvalidQuery))
            })?;
            let mut out = Vec::new();
            for item in rows {
                out.push(item?);
            }
            out
        };
        raws.into_iter().map(|raw| self.build_mask(raw)).collect()
    }

    /// Every mask a profile may bind a shared provider through: its own masks
    /// plus masks other profiles have shared. Owned masks come first.
    pub fn list_masking_servers_for(&self, profile_id: &str) -> Result<Vec<MaskingServer>> {
        let raws: Vec<RawMaskingServer> = {
            let conn = self.conn();
            let mut stmt = conn.prepare(&format!(
                "SELECT {} FROM masking_servers
                 WHERE profile_id = ?1 OR shared = 1
                 ORDER BY (profile_id = ?1) DESC, name",
                mask_columns("")
            ))?;
            let rows = stmt.query_map((profile_id,), |row| {
                raw_mask_from_row(row, 0).and_then(|m| m.ok_or(rusqlite::Error::InvalidQuery))
            })?;
            let mut out = Vec::new();
            for item in rows {
                out.push(item?);
            }
            out
        };
        raws.into_iter().map(|raw| self.build_mask(raw)).collect()
    }

    /// Publish (or unpublish) a mask across the instance.
    pub fn set_mask_shared(&self, mask_id: i64, shared: bool) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE masking_servers SET shared = ?1 WHERE id = ?2",
                (shared as i64, mask_id),
            )
            .context("updating the mask's sharing flag")?;
        if changed == 0 {
            bail!("no masking server matches id {mask_id}");
        }
        Ok(())
    }

    /// A single mask by id.
    pub fn get_masking_server(&self, id: i64) -> Result<Option<MaskingServer>> {
        let raw: Option<RawMaskingServer> = {
            let conn = self.conn();
            conn.query_row(
                &format!(
                    "SELECT {} FROM masking_servers WHERE id = ?1",
                    mask_columns("")
                ),
                (id,),
                |row| {
                    raw_mask_from_row(row, 0).and_then(|m| m.ok_or(rusqlite::Error::InvalidQuery))
                },
            )
            .optional()?
        };
        match raw {
            Some(raw) => Ok(Some(self.build_mask(raw)?)),
            None => Ok(None),
        }
    }

    /// A single mask by name within a profile.
    pub fn get_masking_server_named(
        &self,
        profile_id: &str,
        name: &str,
    ) -> Result<Option<MaskingServer>> {
        let raw: Option<RawMaskingServer> = {
            let conn = self.conn();
            conn.query_row(
                &format!(
                    "SELECT {} FROM masking_servers WHERE profile_id = ?1 AND name = ?2",
                    mask_columns("")
                ),
                (profile_id, name),
                |row| {
                    raw_mask_from_row(row, 0).and_then(|m| m.ok_or(rusqlite::Error::InvalidQuery))
                },
            )
            .optional()?
        };
        match raw {
            Some(raw) => Ok(Some(self.build_mask(raw)?)),
            None => Ok(None),
        }
    }

    /// Update a mask's mutable fields, re-encrypting the supplied secret.
    pub fn update_masking_server(&self, id: i64, spec: NewMaskingServer) -> Result<()> {
        let key = self.master_key();
        let enc_secret = crate::crypto::encrypt(&key, &spec.secret)?;
        let changed = self
            .conn()
            .execute(
                "UPDATE masking_servers
                 SET name = ?1, kind = ?2, endpoint_url = ?3, secret = ?4,
                     max_body_bytes = ?5, expected_egress_ip = ?6, shared = ?7
                 WHERE id = ?8",
                (
                    spec.name.as_str(),
                    spec.kind.as_str(),
                    spec.endpoint_url.as_str(),
                    enc_secret,
                    spec.max_body_bytes.max(0),
                    spec.expected_egress_ip.as_deref(),
                    spec.shared as i64,
                    id,
                ),
            )
            .context("updating the masking server")?;
        if changed == 0 {
            bail!("no masking server matches id {id}");
        }
        Ok(())
    }

    /// Delete a mask. Providers bound to it are unbound rather than deleted: the
    /// binding column is `ON DELETE SET NULL`, so removing a hop never takes
    /// providers (or their credentials) with it.
    pub fn delete_masking_server(&self, id: i64) -> Result<()> {
        let changed = self
            .conn()
            .execute("DELETE FROM masking_servers WHERE id = ?1", (id,))
            .context("deleting the masking server")?;
        if changed == 0 {
            bail!("no masking server matches id {id}");
        }
        Ok(())
    }

    /// Record the egress identity a mask last reported through its probe.
    pub fn set_masking_server_probe(
        &self,
        id: i64,
        ip: &str,
        asn: Option<&str>,
        country: Option<&str>,
    ) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE masking_servers
                 SET last_verified_ip = ?1, last_verified_asn = ?2,
                     last_verified_country = ?3, last_verified_at = ?4
                 WHERE id = ?5",
                (ip, asn, country, now_millis(), id),
            )
            .context("recording the masking server probe")?;
        if changed == 0 {
            bail!("no masking server matches id {id}");
        }
        Ok(())
    }

    /// Set (or clear, with `None`) a profile's default egress mask, used by
    /// providers that do not bind one of their own.
    pub fn set_profile_default_masking(
        &self,
        profile_id: &str,
        mask_id: Option<i64>,
    ) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE profiles SET default_masking_server_id = ?1, updated_at = ?2 WHERE id = ?3",
                (mask_id, now_millis(), profile_id),
            )
            .context("setting the profile default masking server")?;
        if changed == 0 {
            bail!("no profile matches id {profile_id:?}");
        }
        Ok(())
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
                "UPDATE providers SET name = ?1, description = ?2, base_url = ?3, auth_token = ?4, kind = ?5, extra_headers = ?6, masking_server_id = ?7, shared = ?8 WHERE id = ?9",
                (
                    spec.name.as_str(),
                    spec.description.as_deref(),
                    spec.base_url.as_str(),
                    encrypted_token,
                    provider_kind_tag(spec.kind),
                    serde_json::to_string(&spec.extra_headers).expect("BTreeMap<String,String> always serializable"),
                    spec.masking_server_id,
                    spec.shared as i64,
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
        identity: Option<&str>,
    ) -> Result<Route> {
        let _ = self.conn()
            .execute(
                "INSERT INTO routes (proxy_id, name, description, strategy, identity, max_tokens, cache_ttl_secs) VALUES (?1, ?2, ?3, ?4, ?5, 0, 0)",
                (proxy_id, name, description, strategy_tag(strategy), identity),
            )
            .context("inserting the route")?;
        Ok(Route {
            id: self.conn().last_insert_rowid(),
            proxy_id,
            name: name.to_string(),
            description: description.map(|d| d.to_string()),
            strategy,
            identity: identity.map(|i| i.to_string()),
            max_tokens: 0,
            cache_ttl_secs: 0,
            shared: false,
            prompt_cache: PromptCachePolicy::default(),
        })
    }

    /// Publish (or unpublish) a route across the instance, so other profiles
    /// may add it to their own chains (route-as-model).
    pub fn set_route_shared(&self, route_id: i64, shared: bool) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE routes SET shared = ?1 WHERE id = ?2",
                (shared as i64, route_id),
            )
            .context("updating the route's sharing flag")?;
        if changed == 0 {
            bail!("no route matches id {route_id}");
        }
        Ok(())
    }

    /// Set a route's prompt-cache policy (`auto` | `off`).
    pub fn set_route_prompt_cache(&self, route_id: i64, policy: PromptCachePolicy) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE routes SET prompt_cache = ?1 WHERE id = ?2",
                (policy.tag(), route_id),
            )
            .context("updating the route's prompt-cache policy")?;
        if changed == 0 {
            bail!("no route matches id {route_id}");
        }
        Ok(())
    }

    /// Clear (or restore) the `vision` flag on a route entry.
    ///
    /// Used when an upstream answers that it cannot serve images *at all*: the
    /// entry is not sick — it serves text fine — so instead of parking a working
    /// key the router records the missing capability here. That entry is then
    /// skipped for image requests while every other request keeps routing to it.
    pub fn set_route_entry_vision(&self, entry_id: i64, vision: bool) -> Result<()> {
        let conn = self.conn();
        let caps_json: Option<String> = conn
            .query_row(
                "SELECT capabilities FROM route_entries WHERE id = ?1",
                (entry_id,),
                |row| row.get(0),
            )
            .optional()
            .context("reading the route model capabilities")?;
        let Some(caps_json) = caps_json else {
            return Ok(());
        };
        let mut caps: RouteCapabilities = serde_json::from_str(&caps_json).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "invalid capabilities in store, resetting to defaults");
            RouteCapabilities::default()
        });
        if caps.vision == vision {
            return Ok(());
        }
        caps.vision = vision;
        conn.execute(
            "UPDATE route_entries SET capabilities = ?1 WHERE id = ?2",
            (serde_json::to_string(&caps)?, entry_id),
        )
        .context("updating the route model capabilities")?;
        Ok(())
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
        identity: Option<&str>,
    ) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE routes SET name = ?1, description = ?2, strategy = ?3, identity = ?5 WHERE id = ?4",
                (name, description, strategy_tag(strategy), id, identity),
            )
            .context("updating the route")?;
        if changed == 0 {
            bail!("no route matches id {id}");
        }
        Ok(())
    }

    /// Tune economy limits for a route: max_tokens clamp (0 = passthrough)
    /// and exact-cache TTL in seconds (0 = disabled).
    pub fn set_route_economy(&self, id: i64, max_tokens: u32, cache_ttl_secs: i64) -> Result<()> {
        let changed = self
            .conn()
            .execute(
                "UPDATE routes SET max_tokens = ?1, cache_ttl_secs = ?2 WHERE id = ?3",
                (max_tokens as i64, cache_ttl_secs, id),
            )
            .context("updating route economy settings")?;
        if changed == 0 {
            bail!("no route matches id {id}");
        }
        Ok(())
    }

    /// All routes under a proxy.
    pub fn list_routes(&self, proxy_id: i64) -> Result<Vec<Route>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, proxy_id, name, description, strategy, identity, max_tokens, cache_ttl_secs, shared, prompt_cache FROM routes WHERE proxy_id = ?1 ORDER BY name",
        )?;
        let rows = stmt.query_map((proxy_id,), map_route_row)?;
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
                "SELECT id, proxy_id, name, description, strategy, identity, max_tokens, cache_ttl_secs, shared, prompt_cache FROM routes WHERE proxy_id = ?1 AND name = ?2",
                (proxy_id, name),
                map_route_row,
            )
            .optional()
            .map_err(|e| e.into())
    }

    /// Find a route by id, regardless of which profile owns it.
    ///
    /// Callers doing cross-profile work (route-as-model expansion) must still
    /// verify the owning proxy's profile through [`Store::get_proxy`]; this
    /// lookup deliberately does not scope by profile.
    pub fn get_route_by_id(&self, id: i64) -> Result<Option<Route>> {
        self.conn()
            .query_row(
                "SELECT id, proxy_id, name, description, strategy, identity, max_tokens, cache_ttl_secs, shared, prompt_cache FROM routes WHERE id = ?1",
                (id,),
                map_route_row,
            )
            .optional()
            .map_err(|e| e.into())
    }

    /// The profile that owns a route (through its proxy).
    pub fn route_owner_profile(&self, route_id: i64) -> Result<Option<String>> {
        self.conn()
            .query_row(
                "SELECT p.profile_id FROM proxies p JOIN routes r ON r.proxy_id = p.id WHERE r.id = ?1",
                (route_id,),
                |row| row.get(0),
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
                "INSERT INTO route_entries (route_id, provider_id, target_route_id, model_id, priority, weight, status, capabilities, price_per_1m)
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8)",
                (
                    route_id,
                    provider_id,
                    model_id,
                    priority,
                    weight,
                    status_tag(ModelStatus::Healthy),
                    serde_json::to_string(&capabilities).expect("RouteCapabilities always serializable"),
                    default_price_for(model_id),
                ),
            )
            .context("inserting the route model")?;
        Ok(RouteEntry {
            id: self.conn().last_insert_rowid(),
            route_id,
            provider_id: Some(provider_id),
            target_route_id: None,
            model_id: model_id.to_string(),
            priority,
            weight,
            status: ModelStatus::Healthy,
            capabilities,
            price_per_1m: default_price_for(model_id),
            cooldown_until: 0,
        })
    }

    /// Append a nested route to a route's fallback chain (route-as-model).
    ///
    /// `label` is the display name recorded in `model_id`, e.g.
    /// `programmer/php-dev`; the router ignores it during expansion.
    pub fn add_route_entry_ref(
        &self,
        route_id: i64,
        target_route_id: i64,
        label: &str,
        priority: i32,
        weight: f64,
    ) -> Result<RouteEntry> {
        let caps = RouteCapabilities::default();
        let _ = self.conn()
            .execute(
                "INSERT INTO route_entries (route_id, provider_id, target_route_id, model_id, priority, weight, status, capabilities, price_per_1m)
                 VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
                (
                    route_id,
                    target_route_id,
                    label,
                    priority,
                    weight,
                    status_tag(ModelStatus::Healthy),
                    serde_json::to_string(&caps).expect("RouteCapabilities always serializable"),
                ),
            )
            .context("inserting the nested route entry")?;
        Ok(RouteEntry {
            id: self.conn().last_insert_rowid(),
            route_id,
            provider_id: None,
            target_route_id: Some(target_route_id),
            model_id: label.to_string(),
            priority,
            weight,
            status: ModelStatus::Healthy,
            capabilities: caps,
            price_per_1m: 0.0,
            cooldown_until: 0,
        })
    }

    /// Set the blended price (USD / 1M tokens) used by `Economy` sorting.
    pub fn set_route_entry_price(&self, entry_id: i64, price_per_1m: f64) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE route_entries SET price_per_1m = ?1 WHERE id = ?2",
                (price_per_1m.max(0.0), entry_id),
            )
            .context("updating the route model price")?;
        Ok(())
    }

    /// All model entries in a route's chain, ordered by priority.
    pub fn route_entries(&self, route_id: i64) -> Result<Vec<RouteEntry>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, route_id, provider_id, target_route_id, model_id, priority, weight, status, capabilities, price_per_1m, cooldown_until
             FROM route_entries WHERE route_id = ?1 ORDER BY priority",
        )?;
        let rows = stmt.query_map((route_id,), map_route_entry_row)?;
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

    /// Set (or clear, with 0) the rate-limit cooldown for an entry.
    ///
    /// `until` is unix millis. Cooldown is tracked separately from
    /// [`ModelStatus`] so a background probe marking a key healthy cannot cancel
    /// an upstream rate-limit window.
    pub fn set_route_entry_cooldown(&self, entry_id: i64, until: i64) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE route_entries SET cooldown_until = ?1 WHERE id = ?2",
                (until.max(0), entry_id),
            )
            .context("updating the route model cooldown")?;
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
    ///
    /// Each provider is returned with its egress mask attached, so a probe leaves
    /// through the same hop as live traffic. Probing directly would mark every
    /// masked key unhealthy as soon as the mask is the working path.
    pub fn entries_needing_probe(&self) -> Result<Vec<(RouteEntry, Provider)>> {
        let raws: Vec<(RouteEntry, RawProviderRow)> = {
            let conn = self.conn();
            let mut stmt = conn.prepare(&format!(
                "SELECT e.id, e.route_id, e.provider_id, e.target_route_id, e.model_id, e.priority,
                        e.weight, e.status, e.capabilities, e.price_per_1m,
                        e.cooldown_until,
                        {}
                 FROM route_entries e
                 JOIN providers p ON p.id = e.provider_id
                 {}
                 WHERE e.status != ?1 AND e.status != ?2
                   AND e.provider_id IS NOT NULL",
                provider_columns(),
                mask_join()
            ))?;
            let rows = stmt.query_map(
                [
                    status_tag(ModelStatus::Healthy),
                    status_tag(ModelStatus::Disabled),
                ],
                |row| {
                    let entry = map_route_entry_row(row)?;
                    Ok((entry, raw_provider_at(row, 11)?))
                },
            )?;
            let mut out = Vec::new();
            for item in rows {
                out.push(item?);
            }
            out
        };
        raws.into_iter()
            .map(|(entry, (provider, mask))| Ok((entry, self.build_provider(provider, mask)?)))
            .collect()
    }

    // ---- usage logging -------------------------------------------------

    /// Append one completed request to the usage log.
    pub fn record_usage(&self, rec: NewUsage) -> Result<UsageRecord> {
        let now = now_millis();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO usage_log
                (profile_id, request_id, route_entry_id, model_id, streamed, success,
                 status_code, error_message, latency_ms, prompt_tokens,
                 completion_tokens, cached_prompt_tokens, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                rec.profile_id,
                rec.request_id,
                rec.route_entry_id,
                rec.model_id,
                rec.streamed as i64,
                rec.success as i64,
                rec.status_code,
                rec.error_message,
                rec.latency_ms,
                rec.prompt_tokens,
                rec.completion_tokens,
                rec.cached_prompt_tokens,
                now,
            ],
        )?;
        let id = conn.last_insert_rowid();
        Ok(UsageRecord {
            id,
            profile_id: rec.profile_id,
            request_id: rec.request_id,
            route_entry_id: rec.route_entry_id,
            model_id: rec.model_id,
            streamed: rec.streamed,
            success: rec.success,
            status_code: rec.status_code,
            error_message: rec.error_message,
            latency_ms: rec.latency_ms,
            prompt_tokens: rec.prompt_tokens,
            completion_tokens: rec.completion_tokens,
            cached_prompt_tokens: rec.cached_prompt_tokens,
            created_at: now,
        })
    }

    /// The most recent usage records for a profile, newest first.
    pub fn list_usage(&self, profile_id: &str, limit: u32) -> Result<Vec<UsageRecord>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, profile_id, request_id, route_entry_id, model_id, streamed, success,
                    status_code, error_message, latency_ms, prompt_tokens,
                    completion_tokens, cached_prompt_tokens, created_at
             FROM usage_log
             WHERE profile_id = ?1
             ORDER BY created_at DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map([profile_id, &limit.to_string()], |row| {
            Ok(UsageRecord {
                id: row.get(0)?,
                profile_id: row.get(1)?,
                request_id: row.get(2)?,
                route_entry_id: row.get(3)?,
                model_id: row.get(4)?,
                streamed: row.get::<_, i64>(5)? != 0,
                success: row.get::<_, i64>(6)? != 0,
                status_code: row.get(7)?,
                error_message: row.get(8)?,
                latency_ms: row.get(9)?,
                prompt_tokens: row.get(10)?,
                completion_tokens: row.get(11)?,
                cached_prompt_tokens: row.get(12)?,
                created_at: row.get(13)?,
            })
        })?;
        let mut out = Vec::new();
        for item in rows {
            out.push(item?);
        }
        Ok(out)
    }

    /// Per-model aggregates (calls, failures, latency, tokens) for a profile.
    ///
    /// `cached_prompt_tokens` / `cached_calls` come from the upstream's own cache
    /// accounting (`cache_read_input_tokens`, `prompt_tokens_details.cached_tokens`,
    /// `cachedContentTokenCount`), so they measure *provider-side* cache reuse —
    /// distinct from the proxy's own exact-match `response_cache`, which never
    /// reaches an upstream and is reported separately.
    pub fn usage_stats(&self, profile_id: &str) -> Result<Vec<UsageStats>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT model_id,
                    COUNT(*) AS calls,
                    SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END) AS failures,
                    AVG(latency_ms),
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(cached_prompt_tokens), 0) AS cached_tokens,
                    COALESCE(SUM(CASE WHEN cached_prompt_tokens > 0 THEN 1 ELSE 0 END), 0) AS cached_calls
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
                cached_prompt_tokens: row.get(6)?,
                cached_calls: row.get(7)?,
                est_cost_usd: 0.0,
            })
        })?;
        let mut out = Vec::new();
        for item in rows {
            out.push(item?);
        }
        // Attach blended price from the cheapest matching entry (best-effort).
        for s in &mut out {
            let price: Option<f64> = conn
                .query_row(
                    "SELECT MIN(price_per_1m) FROM route_entries WHERE model_id = ?1 AND price_per_1m > 0",
                    [&s.model_id],
                    |r| r.get(0),
                )
                .unwrap_or(None);
            if let Some(p) = price {
                s.est_cost_usd = (s.prompt_tokens + s.completion_tokens) as f64 * p / 1_000_000.0;
            }
        }
        Ok(out)
    }

    /// Per-key (provider) aggregates for a profile, including how often each key
    /// came back `429`.
    ///
    /// This is the dashboard for a profile that spreads several keys of one
    /// upstream across several egress masks: even call counts mean the keys are
    /// genuinely being used in parallel, while a lopsided split means the router
    /// is still leaning on one of them.
    pub fn key_stats(&self, profile_id: &str) -> Result<Vec<KeyStats>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT p.id, p.name, m.name,
                    COUNT(*) AS calls,
                    COALESCE(SUM(CASE WHEN u.success = 0 THEN 1 ELSE 0 END), 0) AS failures,
                    COALESCE(SUM(CASE WHEN u.status_code = 429 THEN 1 ELSE 0 END), 0) AS rate_limited,
                    COALESCE(AVG(u.latency_ms), 0)
             FROM usage_log u
             JOIN route_entries e ON e.id = u.route_entry_id
             JOIN providers p ON p.id = e.provider_id
             {}
             WHERE u.profile_id = ?1
             GROUP BY p.id, m.name
             ORDER BY calls DESC",
            mask_join()
        ))?;
        let rows = stmt.query_map([profile_id], |row| {
            Ok(KeyStats {
                provider_id: row.get(0)?,
                provider_name: row.get(1)?,
                mask_name: row.get(2)?,
                calls: row.get(3)?,
                failures: row.get(4)?,
                rate_limited: row.get(5)?,
                avg_latency_ms: row.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for item in rows {
            out.push(item?);
        }
        Ok(out)
    }

    /// Look up a cached exact response for a route+hash if still fresh.
    /// Returns `(body, prompt_tokens, completion_tokens)`.
    pub fn cache_get(
        &self,
        route_id: i64,
        req_hash: &str,
        now_ms: i64,
    ) -> Result<Option<CacheHit>> {
        // Opportunistically drop expired rows (cheap, keeps table small).
        let _ = self.conn().execute(
            "DELETE FROM response_cache WHERE expires_at <= ?1",
            (now_ms,),
        );
        let row: Option<(Vec<u8>, Option<i64>, Option<i64>)> = self
            .conn()
            .query_row(
                "SELECT resp_json, prompt_tokens, completion_tokens FROM response_cache WHERE route_id = ?1 AND req_hash = ?2 AND expires_at > ?3",
                rusqlite::params![route_id, req_hash, now_ms],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        Ok(row)
    }

    /// Store an exact response for later reuse.
    #[allow(clippy::too_many_arguments)]
    pub fn cache_put(
        &self,
        route_id: i64,
        req_hash: &str,
        resp_json: &[u8],
        prompt_tokens: Option<i64>,
        completion_tokens: Option<i64>,
        now_ms: i64,
        ttl_secs: i64,
    ) -> Result<()> {
        if ttl_secs <= 0 {
            return Ok(());
        }
        let expires = now_ms + ttl_secs * 1000;
        self.conn().execute(
            "INSERT INTO response_cache (route_id, req_hash, resp_json, prompt_tokens, completion_tokens, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(route_id, req_hash) DO UPDATE SET resp_json=excluded.resp_json, prompt_tokens=excluded.prompt_tokens, completion_tokens=excluded.completion_tokens, created_at=excluded.created_at, expires_at=excluded.expires_at",
            rusqlite::params![route_id, req_hash, resp_json, prompt_tokens, completion_tokens, now_ms, expires],
        )?;
        Ok(())
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

    /// Remove expired response cache entries. Returns the number of rows deleted.
    /// Called periodically to prevent unbounded cache growth.
    pub fn prune_response_cache(&self) -> Result<usize> {
        let now = chrono::Utc::now().timestamp_millis();
        let deleted = self
            .conn()
            .execute("DELETE FROM response_cache WHERE expires_at <= ?1", (now,))?;
        Ok(deleted)
    }
}

/// Cached response body plus its recorded upstream token counts.
pub type CacheHit = (Vec<u8>, Option<i64>, Option<i64>);

/// Exercise the full CRUD round-trip against an in-memory store.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ProviderKind, RoutingStrategy};

    #[test]
    fn provider_kind_tags_roundtrip_through_the_db() -> Result<()> {
        let store = Store::open_in_memory()?;
        let profile = store.create_profile("kinds", None, None)?;
        let kinds = [
            (ProviderKind::OpenAI, "openai"),
            (ProviderKind::Anthropic, "anthropic"),
            (ProviderKind::Google, "google"),
            (ProviderKind::OpenAIResponses, "openai_responses"),
            (ProviderKind::Custom, "custom"),
        ];
        for (i, (kind, tag)) in kinds.iter().enumerate() {
            store.create_provider(
                profile.id.as_str(),
                NewProvider {
                    name: format!("p{i}"),
                    description: None,
                    base_url: "https://up.test".to_string(),
                    auth_token: "tok".to_string(),
                    kind: *kind,
                    extra_headers: Default::default(),
                    masking_server_id: None,
                    shared: false,
                },
            )?;
            let listed = store.list_providers(profile.id.as_str())?;
            let stored = listed.iter().find(|p| p.name == format!("p{i}")).unwrap();
            assert_eq!(stored.kind, *kind, "kind must survive the roundtrip");
            assert_eq!(provider_kind_tag(stored.kind), *tag);
        }
        Ok(())
    }

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
                name: "provider-a".to_string(),
                description: None,
                base_url: "https://api.example.com".to_string(),
                auth_token: "sk-secret".to_string(),
                kind: ProviderKind::OpenAI,
                extra_headers: headers,
                masking_server_id: None,
                shared: false,
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
                kind: ProviderKind::OpenAI,
                extra_headers: std::collections::BTreeMap::new(),
                masking_server_id: None,
                shared: false,
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
                masking_server_id: None,
                shared: false,
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
        let route = store.create_route(proxy.id, "r", None, RoutingStrategy::Priority, None)?;
        store.update_route(
            route.id,
            "r2",
            Some("desc"),
            RoutingStrategy::Weighted,
            None,
        )?;
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

        let provider_a = store.create_provider(
            profile.id.as_str(),
            NewProvider {
                name: "provider-a".to_string(),
                description: None,
                base_url: "https://api.example.com".to_string(),
                auth_token: "a".to_string(),
                kind: ProviderKind::OpenAI,
                extra_headers: std::collections::BTreeMap::new(),
                masking_server_id: None,
                shared: false,
            },
        )?;
        let provider_b = store.create_provider(
            profile.id.as_str(),
            NewProvider {
                name: "provider-b".to_string(),
                description: None,
                base_url: "https://upstream.example".to_string(),
                auth_token: "b".to_string(),
                kind: ProviderKind::OpenAI,
                extra_headers: std::collections::BTreeMap::new(),
                masking_server_id: None,
                shared: false,
            },
        )?;

        let proxy = store.create_proxy(profile.id.as_str(), "Programmer", None)?;
        let route = store.create_route(
            proxy.id,
            "php-developer-3.5-flash",
            None,
            RoutingStrategy::Priority,
            None,
        )?;

        let second = store.add_route_entry(
            route.id,
            provider_b.id,
            "provider-flash",
            2,
            1.0,
            RouteCapabilities::default(),
        )?;
        let first = store.add_route_entry(
            route.id,
            provider_a.id,
            "example-model",
            1,
            1.0,
            RouteCapabilities::default(),
        )?;

        let chain = store.route_entries(route.id)?;
        // Insertion order was reversed, but the chain must surface priority order.
        assert_eq!(chain[0].provider_id, Some(provider_a.id));
        assert_eq!(chain[0].id, first.id);
        assert_eq!(chain[1].provider_id, Some(provider_b.id));
        assert_eq!(chain[1].id, second.id);

        store.set_route_entry_status(first.id, ModelStatus::Unhealthy)?;
        let updated = store.route_entries(route.id)?;
        assert_eq!(updated[0].status, ModelStatus::Unhealthy);
        Ok(())
    }

    #[test]
    fn economy_cache_roundtrip_and_price_estimate() -> Result<()> {
        let store = Store::open_in_memory()?;
        let profile = store.create_profile("eco", None, None)?;
        let provider = store.create_provider(
            profile.id.as_str(),
            NewProvider {
                name: "p".to_string(),
                description: None,
                base_url: "https://a.example".to_string(),
                auth_token: "t".to_string(),
                kind: ProviderKind::OpenAI,
                extra_headers: std::collections::BTreeMap::new(),
                masking_server_id: None,
                shared: false,
            },
        )?;
        let proxy = store.create_proxy(profile.id.as_str(), "prog", None)?;
        let route = store.create_route(proxy.id, "r", None, RoutingStrategy::Economy, None)?;
        store.set_route_economy(route.id, 1024, 3600)?;
        let got = store.get_route_named(proxy.id, "r")?.unwrap();
        assert_eq!(got.max_tokens, 1024);
        assert_eq!(got.cache_ttl_secs, 3600);

        let entry = store.add_route_entry(
            route.id,
            provider.id,
            "provider-mini",
            1,
            1.0,
            RouteCapabilities::default(),
        )?;
        assert!(entry.price_per_1m > 0.0); // auto-guessed cheap price
        store.set_route_entry_price(entry.id, 0.4)?;
        assert_eq!(store.route_entries(route.id)?[0].price_per_1m, 0.4);

        // Cache put/get.
        let now = chrono::Utc::now().timestamp_millis();
        assert!(store.cache_get(route.id, "h1", now)?.is_none());
        store.cache_put(
            route.id,
            "h1",
            b"{\"ok\":true}",
            Some(10),
            Some(5),
            now,
            3600,
        )?;
        let hit = store.cache_get(route.id, "h1", now)?.unwrap();
        assert_eq!(hit.0, b"{\"ok\":true}");
        // Expired.
        assert!(store
            .cache_get(route.id, "h1", now + 3600 * 1000 + 1)?
            .is_none());
        Ok(())
    }

    #[test]
    fn default_price_heuristics() {
        assert!(crate::storage::default_price_for("provider-mini") < 1.0);
        assert!(crate::storage::default_price_for("provider-micro") < 1.0);
        assert!(crate::storage::default_price_for("provider-pro") > 5.0);
        assert!(crate::storage::default_price_for("provider-flash") < 1.0);
    }

    // --- egress masks -------------------------------------------------------

    fn new_mask(name: &str) -> NewMaskingServer {
        NewMaskingServer {
            name: name.to_string(),
            kind: "vps".to_string(),
            endpoint_url: format!("https://{name}.example/forward"),
            secret: format!("secret-for-{name}"),
            max_body_bytes: 0,
            expected_egress_ip: None,
            shared: false,
        }
    }

    fn new_provider(name: &str, mask_id: Option<i64>) -> NewProvider {
        NewProvider {
            name: name.to_string(),
            description: None,
            base_url: "https://upstream.example".to_string(),
            auth_token: "up-key".to_string(),
            kind: ProviderKind::OpenAI,
            extra_headers: Default::default(),
            masking_server_id: mask_id,
            shared: false,
        }
    }

    #[test]
    fn a_shared_provider_is_visible_to_other_profiles() -> Result<()> {
        let store = Store::open_in_memory()?;
        let owner = store.create_profile("owner", None, None)?;
        let other = store.create_profile("other", None, None)?;
        let provider =
            store.create_provider(owner.id.as_str(), new_provider("shared-key", None))?;

        // Not shared yet: only the owner sees it.
        assert_eq!(store.list_providers(owner.id.as_str())?.len(), 1);
        assert!(store.list_providers_for(other.id.as_str())?.is_empty());

        store.set_provider_shared(provider.id, true)?;
        let visible = store.list_providers_for(other.id.as_str())?;
        assert_eq!(visible.len(), 1, "shared provider must be visible");
        assert!(visible[0].shared);
        assert_eq!(visible[0].name, "shared-key");

        // The owner's own listing is unchanged (it never gained a duplicate).
        assert_eq!(store.list_providers(owner.id.as_str())?.len(), 1);

        // Unpublishing hides it again.
        store.set_provider_shared(provider.id, false)?;
        assert!(store.list_providers_for(other.id.as_str())?.is_empty());
        Ok(())
    }

    #[test]
    fn sharing_flag_survives_provider_update_and_roundtrips_export() -> Result<()> {
        let store = Store::open_in_memory()?;
        let profile = store.create_profile("p", None, None)?;
        let provider = store.create_provider(profile.id.as_str(), new_provider("k", None))?;
        assert!(!provider.shared);

        store.update_provider(
            provider.id,
            NewProvider {
                shared: true,
                ..new_provider("k", None)
            },
        )?;
        assert!(store.get_provider(provider.id)?.unwrap().shared);

        // A mask follows the same pattern.
        let mask = store.create_masking_server(
            profile.id.as_str(),
            crate::storage::NewMaskingServer {
                shared: true,
                ..new_mask("edge")
            },
        )?;
        assert!(mask.shared);
        assert_eq!(
            store.list_masking_servers_for(profile.id.as_str())?.len(),
            1
        );
        Ok(())
    }

    #[test]
    fn a_mask_binds_to_a_provider_and_travels_with_it() -> Result<()> {
        let store = Store::open_in_memory()?;
        let profile = store.create_profile("masked", None, None)?;
        let mask = store.create_masking_server(profile.id.as_str(), new_mask("edge-1"))?;
        let provider =
            store.create_provider(profile.id.as_str(), new_provider("key-1", Some(mask.id)))?;

        // The secret is decrypted for egress use, but never serialized.
        assert_eq!(
            provider.masking_server.as_ref().unwrap().secret,
            "secret-for-edge-1"
        );
        assert!(!serde_json::to_string(&provider)?.contains("secret-for-edge-1"));

        // Every read path agrees, including the one health probing uses.
        let loaded = store.get_provider(provider.id)?.unwrap();
        assert_eq!(loaded.masking_server_id, Some(mask.id));
        assert_eq!(loaded.masking_server.as_ref().unwrap().name, "edge-1");
        let listed = store.list_providers(profile.id.as_str())?;
        assert_eq!(
            listed[0].masking_server.as_ref().unwrap().endpoint_url,
            "https://edge-1.example/forward"
        );
        Ok(())
    }

    #[test]
    fn five_keys_can_leave_from_five_masks() -> Result<()> {
        // The point of the feature: one egress identity per key.
        let store = Store::open_in_memory()?;
        let profile = store.create_profile("fleet", None, None)?;
        let mut endpoints = Vec::new();
        for i in 1..=5 {
            let mask =
                store.create_masking_server(profile.id.as_str(), new_mask(&format!("key-{i}")))?;
            let provider = store.create_provider(
                profile.id.as_str(),
                new_provider(&format!("key-{i}"), Some(mask.id)),
            )?;
            endpoints.push(
                provider
                    .masking_server
                    .expect("each key carries its own mask")
                    .endpoint_url,
            );
        }
        endpoints.sort();
        endpoints.dedup();
        assert_eq!(endpoints.len(), 5, "five keys must present five hops");
        Ok(())
    }

    #[test]
    fn the_profile_default_covers_providers_that_bind_nothing() -> Result<()> {
        let store = Store::open_in_memory()?;
        let profile = store.create_profile("defaulted", None, None)?;
        let mask = store.create_masking_server(profile.id.as_str(), new_mask("shared"))?;
        store.set_profile_default_masking(profile.id.as_str(), Some(mask.id))?;

        let bound = store.create_provider(profile.id.as_str(), new_provider("key-1", None))?;
        assert_eq!(bound.masking_server.as_ref().unwrap().name, "shared");
        // The explicit binding stays empty, so `provider edit` shows the truth.
        assert_eq!(bound.masking_server_id, None);

        // A provider-level binding wins over the profile default.
        let own = store.create_masking_server(profile.id.as_str(), new_mask("own"))?;
        let overriding =
            store.create_provider(profile.id.as_str(), new_provider("key-2", Some(own.id)))?;
        assert_eq!(overriding.masking_server.as_ref().unwrap().name, "own");

        // Clearing the default returns everyone to direct egress.
        store.set_profile_default_masking(profile.id.as_str(), None)?;
        let after = store.get_provider(bound.id)?.unwrap();
        assert!(after.masking_server.is_none());
        Ok(())
    }

    #[test]
    fn deleting_a_mask_unbinds_providers_without_deleting_them() -> Result<()> {
        let store = Store::open_in_memory()?;
        let profile = store.create_profile("cleanup", None, None)?;
        let mask = store.create_masking_server(profile.id.as_str(), new_mask("edge-1"))?;
        let provider =
            store.create_provider(profile.id.as_str(), new_provider("key-1", Some(mask.id)))?;

        store.delete_masking_server(mask.id)?;

        // The provider survives with its credentials intact, merely unbound.
        let survivor = store
            .get_provider(provider.id)?
            .expect("deleting a mask must never delete a provider");
        assert_eq!(survivor.masking_server_id, None);
        assert!(survivor.masking_server.is_none());
        assert_eq!(survivor.auth_token, "up-key");
        Ok(())
    }

    #[test]
    fn mask_updates_and_probe_results_roundtrip() -> Result<()> {
        let store = Store::open_in_memory()?;
        let profile = store.create_profile("probed", None, None)?;
        let mask = store.create_masking_server(profile.id.as_str(), new_mask("edge-1"))?;

        let mut spec = new_mask("edge-1");
        spec.secret = "rotated".to_string();
        spec.max_body_bytes = 6 * 1024 * 1024;
        store.update_masking_server(mask.id, spec)?;
        store.set_masking_server_probe(mask.id, "203.0.113.7", Some("AS24940"), Some("DE"))?;

        let reloaded = store.get_masking_server(mask.id)?.unwrap();
        assert_eq!(reloaded.secret, "rotated", "rotation must take effect");
        assert_eq!(reloaded.max_body_bytes, 6 * 1024 * 1024);
        assert_eq!(reloaded.last_verified_ip.as_deref(), Some("203.0.113.7"));
        assert_eq!(reloaded.last_verified_asn.as_deref(), Some("AS24940"));
        assert!(reloaded.last_verified_at.is_some());

        // Name lookups (used by tooling) agree with id lookups.
        let by_name = store
            .get_masking_server_named(profile.id.as_str(), "edge-1")?
            .expect("a mask is addressable by name");
        assert_eq!(by_name.id, mask.id);
        assert_eq!(store.list_masking_servers(profile.id.as_str())?.len(), 1);
        Ok(())
    }

    #[test]
    fn a_rate_limited_entry_can_be_cooled_down_and_warmed_back_up() -> Result<()> {
        let store = Store::open_in_memory()?;
        let profile = store.create_profile("cooling", None, None)?;
        let provider = store.create_provider(profile.id.as_str(), new_provider("key-1", None))?;
        let proxy = store.create_proxy(profile.id.as_str(), "p", None)?;
        let route = store.create_route(proxy.id, "r", None, RoutingStrategy::RoundRobin, None)?;
        let entry = store.add_route_entry(
            route.id,
            provider.id,
            "m",
            1,
            1.0,
            RouteCapabilities::default(),
        )?;
        assert_eq!(entry.cooldown_until, 0, "new entries start warm");

        let until = chrono::Utc::now().timestamp_millis() + 30_000;
        store.set_route_entry_cooldown(entry.id, until)?;
        assert_eq!(store.route_entries(route.id)?[0].cooldown_until, until);

        // Cooling is independent of health, so a probe cannot cancel it.
        store.set_route_entry_status(entry.id, ModelStatus::Healthy)?;
        assert_eq!(store.route_entries(route.id)?[0].cooldown_until, until);

        store.set_route_entry_cooldown(entry.id, 0)?;
        assert_eq!(store.route_entries(route.id)?[0].cooldown_until, 0);
        Ok(())
    }
}

//! Background health probes.
//!
//! A single background task periodically pings every route entry that isn't
//! Healthy or Disabled, recovers the ones that respond, and demotes the ones
//! that don't. Failover is useless without this loop — without it, once an entry
//! is marked Unhealthy it stays dead until a human intervenes.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::time::interval;
use tracing::info_span;

use crate::domain::ModelStatus;
use crate::storage::Store;

/// How often the probe loop wakes up and scans for entries to check.
const PROBE_INTERVAL: Duration = Duration::from_secs(30);

/// Lightweight ping request: ask the upstream for its model list. Cheap, fast,
/// and works across every OpenAI-compatible provider.
const PING_PATH: &str = "/v1/models";

/// Start the background health-probe runner. Returns a JoinHandle so the caller
/// can abort it on shutdown.
pub fn spawn(store: Arc<Store>, http_client: reqwest::Client) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = interval(PROBE_INTERVAL);
        ticker.tick().await; // first tick fires immediately
        loop {
            ticker.tick().await;
            if let Err(e) = run_once(&store, &http_client).await {
                tracing::warn!(error = %e, "health probe iteration failed");
            }
        }
    })
}

/// One probe cycle: fetch candidates, ping each, update status.
async fn run_once(store: &Store, client: &reqwest::Client) -> Result<()> {
    let candidates = tokio::task::block_in_place(|| store.entries_needing_probe())?;
    if candidates.is_empty() {
        return Ok(());
    }
    tracing::debug!(count = candidates.len(), "probing entries");
    for (entry, provider) in candidates {
        let span = info_span!("probe", entry_id = entry.id, provider = %provider.name);
        let _guard = span.enter();
        let outcome = ping(client, &provider).await;
        let new_status = decide_status(&entry.status, outcome);
        if new_status != entry.status {
            tokio::task::block_in_place(|| {
                store.set_route_entry_status(entry.id, new_status)
            })?;
            tracing::info!(old = ?entry.status, new = ?new_status, "entry status changed");
        }
    }
    Ok(())
}

/// Send a lightweight ping to the provider. Returns true on any success.
async fn ping(client: &reqwest::Client, provider: &crate::domain::Provider) -> bool {
    let base = provider.base_url.trim_end_matches('/');
    let url = format!("{base}{PING_PATH}");
    let mut req = client.get(&url);
    if !provider.auth_token.is_empty() {
        req = req.bearer_auth(&provider.auth_token);
    }
    match req.send().await {
        Ok(resp) => {
            let ok = resp.status().is_success();
            if !ok {
                tracing::debug!(status = %resp.status(), "ping non-success");
            }
            ok
        }
        Err(e) => {
            tracing::debug!(error = %e, "ping failed");
            false
        }
    }
}

/// State machine for status transitions on each probe outcome.
///
/// - Healthy stays Healthy on success (no-op).
/// - Degraded recovers to Healthy on success, demotes to Unhealthy on failure.
/// - Unhealthy recovers to Degraded on success (must prove itself once before
///   being promoted all the way back to Healthy).
fn decide_status(current: &ModelStatus, success: bool) -> ModelStatus {
    match (current, success) {
        // Healthy stays Healthy regardless (failure on a healthy entry keeps it
        // in rotation — only live traffic failures demote it).
        (ModelStatus::Healthy, _) => ModelStatus::Healthy,
        (ModelStatus::Degraded, true) => ModelStatus::Healthy,
        (ModelStatus::Degraded, false) => ModelStatus::Unhealthy,
        (ModelStatus::Unhealthy, true) => ModelStatus::Degraded,
        (ModelStatus::Unhealthy, false) => ModelStatus::Unhealthy,
        // Disabled is never probed, but handle it defensively.
        (ModelStatus::Disabled, _) => ModelStatus::Disabled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_status_recovers_through_degraded() {
        // Unhealthy → Degraded on first success.
        assert_eq!(decide_status(&ModelStatus::Unhealthy, true), ModelStatus::Degraded);
        // Degraded → Healthy on next success.
        assert_eq!(decide_status(&ModelStatus::Degraded, true), ModelStatus::Healthy);
        // Healthy stays Healthy.
        assert_eq!(decide_status(&ModelStatus::Healthy, true), ModelStatus::Healthy);
    }

    #[test]
    fn decide_status_degrades_on_failure() {
        // Degraded → Unhealthy on failure.
        assert_eq!(decide_status(&ModelStatus::Degraded, false), ModelStatus::Unhealthy);
        // Unhealthy stays Unhealthy on failure.
        assert_eq!(decide_status(&ModelStatus::Unhealthy, false), ModelStatus::Unhealthy);
    }

    #[test]
    fn decide_status_disabled_is_untouched() {
        assert_eq!(decide_status(&ModelStatus::Disabled, true), ModelStatus::Disabled);
        assert_eq!(decide_status(&ModelStatus::Disabled, false), ModelStatus::Disabled);
    }
}
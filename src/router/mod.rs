//! Request routing and priority failover.
//!
//! Given a model string (e.g. `programmer/php-developer-3.5-flash`), resolve the
//! route and try each healthy entry in priority order until one succeeds.

use std::sync::Arc;

use anyhow::{bail, Context as _, Result};
use tokio::time::{timeout, Duration};

use crate::domain::{ModelStatus, Provider, RouteEntry};
use crate::storage::Store;

/// A resolved target: a specific provider + model to try next.
#[derive(Debug, Clone)]
pub struct Target {
    pub provider: Provider,
    pub entry: RouteEntry,
}

/// Resolve a model string to an ordered list of healthy targets.
pub fn resolve_targets(store: &Store, model: &str) -> Result<Vec<Target>> {
    let (proxy_name, route_name) = model
        .split_once('/')
        .with_context(|| format!("model {model:?} must be in `<proxy>/<route>` format"))?;

    let profiles = store.list_profiles()?;
    let mut found = None;
    for profile in &profiles {
        if let Some(proxy) = store.get_proxy_named(profile.id.as_str(), proxy_name)? {
            found = Some((profile.id.clone(), proxy));
            break;
        }
    }
    let (_profile_id, proxy) = found.with_context(|| format!("no proxy named {proxy_name:?}"))?;

    let route = store
        .get_route_named(proxy.id, route_name)?
        .with_context(|| format!("no route named {route_name:?} under proxy {proxy_name:?}"))?;

    let entries = store.route_entries(route.id)?;
    let mut targets = Vec::new();
    for entry in entries {
        if !matches!(entry.status, ModelStatus::Healthy | ModelStatus::Degraded) {
            continue;
        }
        if let Some(provider) = store.get_provider(entry.provider_id)? {
            targets.push(Target { provider, entry });
        }
    }
    Ok(targets)
}

/// Execute a request against resolved targets with priority failover.
pub async fn execute_with_failover<F, Fut>(
    store: Arc<Store>,
    targets: Vec<Target>,
    attempt_timeout: Duration,
    mut attempt_fn: F,
) -> Result<Vec<u8>>
where
    F: FnMut(Target) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>>>,
{
    if targets.is_empty() {
        bail!("no healthy targets for this route");
    }

    let mut last_error = None;
    for target in &targets {
        match timeout(attempt_timeout, attempt_fn(target.clone())).await {
            Ok(Ok(bytes)) => return Ok(bytes),
            Ok(Err(e)) => {
                last_error = Some(e);
                tracing::warn!(
                    provider = %target.provider.name,
                    model = %target.entry.model_id,
                    "attempt failed, failing over"
                );
            }
            Err(_) => {
                last_error = Some(anyhow::anyhow!("attempt timed out after {attempt_timeout:?}"));
                tracing::warn!(
                    provider = %target.provider.name,
                    model = %target.entry.model_id,
                    "attempt timed out, failing over"
                );
            }
        }
    }

    for target in &targets {
        let _ = store.set_route_entry_status(target.entry.id, ModelStatus::Unhealthy);
    }

    match last_error {
        Some(e) => Err(e),
        None => bail!("all targets failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ProviderKind, RoutingStrategy};
    use crate::storage::NewProvider;
    use std::collections::BTreeMap;

    fn setup() -> (Store, Vec<Target>) {
        let store = Store::open_in_memory().unwrap();
        let profile = store.create_profile("coder1", None, None).unwrap();
        let provider = store
            .create_provider(
                profile.id.as_str(),
                NewProvider {
                    name: "p1".into(),
                    description: None,
                    base_url: "https://example.com".into(),
                    auth_token: "tok".into(),
                    kind: ProviderKind::OpenAICompatible,
                    extra_headers: BTreeMap::new(),
                },
            )
            .unwrap();
        let proxy = store.create_proxy(profile.id.as_str(), "prog", None).unwrap();
        let route = store.create_route(proxy.id, "r1", None, RoutingStrategy::Priority).unwrap();
        store
            .add_route_entry(route.id, provider.id, "m1", 1, 1.0, Default::default())
            .unwrap();
        let targets = resolve_targets(&store, "prog/r1").unwrap();
        assert_eq!(targets.len(), 1);
        (store, targets)
    }

    #[tokio::test]
    async fn failover_returns_first_success() {
        let (store, targets) = setup();
        let store = Arc::new(store);
        let result = execute_with_failover(store, targets, Duration::from_secs(5),|_| async {
            Ok(b"ok".to_vec())
        })
        .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"ok");
    }

    #[tokio::test]
    async fn failover_tries_next_on_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (store, targets) = setup();
        let store = Arc::new(store);
        let attempts = Arc::new(AtomicUsize::new(0));
        let a = attempts.clone();
        let result = execute_with_failover(store, targets, Duration::from_secs(5), move |_t| {
            a.fetch_add(1, Ordering::SeqCst);
            async move { Err::<Vec<u8>, _>(anyhow::anyhow!("boom")) }
        })
        .await;
        // With a single target, it tries once and fails.
        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failover_filters_out_unhealthy_entries() {
        let (store, _) = setup();
        // Re-resolve the route id through the public API to mimic real usage.
        let profiles = store.list_profiles().unwrap();
        let proxies = store.list_proxies(profiles[0].id.as_str()).unwrap();
        let routes = store.list_routes(proxies[0].id).unwrap();
        let entries = store.route_entries(routes[0].id).unwrap();
        assert!(!entries.is_empty(), "expected at least one route entry");
        store
            .set_route_entry_status(entries[0].id, ModelStatus::Unhealthy)
            .unwrap();
        let targets = resolve_targets(&store, "prog/r1").unwrap();
        assert!(targets.is_empty());
    }
}
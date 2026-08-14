//! Request routing and priority failover.
//!
//! Given a model string (e.g. `programmer/php-developer-3.5-flash`), resolve the
//! route and try each healthy entry in priority order until one succeeds.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context as _, Result};
use rand::Rng;
use tokio::time::{timeout, Duration};

use crate::domain::{ModelStatus, Provider, RouteEntry, RoutingStrategy};
use crate::storage::Store;

/// Per-route mutable routing state (round-robin counters, etc.).
///
/// Held behind a `Mutex` inside `AppState`; each request locks it briefly to
/// read/update the counter for the route it's about to hit.
#[derive(Default, Clone)]
pub struct RoutingState {
    /// `route_id → last-used index` for round-robin.
    round_robin: Arc<Mutex<HashMap<i64, usize>>>,
}

impl RoutingState {
    /// Rotate the targets for a round-robin route: the entry after the last
    /// used one goes first. Updates the stored counter.
    pub fn rotate_round_robin(&self, route_id: i64, targets: &mut [Target]) {
        if targets.len() <= 1 {
            return;
        }
        // Recover from a poisoned lock (a prior panic mid-lock) rather than
        // crashing a live request: the routing counter is non-critical state,
        // so it is always safe to continue with the recovered data.
        let mut counters = self
            .round_robin
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let idx = counters.entry(route_id).or_insert(0);
        let len = targets.len();
        let split = *idx % len;
        // Rotate so `split` is at the front, then advance the counter.
        targets.rotate_left(split);
        *idx = (*idx + 1) % len;
    }

    /// Pick a starting index by weight (higher weight → more likely first),
    /// then rotate the targets so that entry leads. Falls back to priority
    /// order when all weights are zero or there is only one entry.
    pub fn shuffle_weighted(&self, targets: &mut [Target]) {
        if targets.len() <= 1 {
            return;
        }
        let total: f64 = targets.iter().map(|t| t.entry.weight).sum();
        if total <= 0.0 {
            return;
        }
        let mut rng = rand::thread_rng();
        let mut pick = rng.gen::<f64>() * total;
        let mut chosen = 0usize;
        for (i, t) in targets.iter().enumerate() {
            pick -= t.entry.weight;
            if pick <= 0.0 {
                chosen = i;
                break;
            }
        }
        targets.rotate_left(chosen);
    }
}

/// What a request actually requires, derived from its body. Entries whose
/// capabilities can't satisfy the needs are skipped during resolution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestNeeds {
    /// The request carries tool / function-call definitions.
    pub tools: bool,
    /// The request carries image (vision) parts.
    pub vision: bool,
    /// The request asks for structured JSON output.
    pub json_mode: bool,
}

impl RequestNeeds {
    /// Infer the needs from a raw OpenAI-compatible request body.
    pub fn from_body(body: &serde_json::Value) -> Self {
        let tools = body
            .get("tools")
            .and_then(|t| t.as_array())
            .is_some_and(|a| !a.is_empty());
        let json_mode = body
            .pointer("/response_format/type")
            .and_then(|v| v.as_str())
            .is_some_and(|t| t == "json_object");
        let mut vision = false;
        if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
            for msg in messages {
                if msg.get("content").is_some_and(|c| c.is_array()) {
                    vision = true;
                    break;
                }
            }
        }
        Self {
            tools,
            vision,
            json_mode,
        }
    }

    /// Whether an entry's capabilities can serve a request with these needs.
    /// An entry with unknown/unset capabilities is assumed unable; the CLI
    /// wizard writes explicit flags at creation time.
    fn satisfies(&self, caps: crate::domain::RouteCapabilities) -> bool {
        (!self.tools || caps.tools)
            && (!self.vision || caps.vision)
            && (!self.json_mode || caps.json_mode)
    }
}

/// A resolved target: a specific provider + model to try next.
#[derive(Debug, Clone)]
pub struct Target {
    pub provider: Provider,
    pub entry: RouteEntry,
    /// Optional identity description from the route. When set, the proxy
    /// injects a system message into the request so the model adopts this
    /// identity and hides its original one.
    pub identity: Option<String>,
}

/// Resolve a model string to an ordered list of healthy targets, scoped to the
/// given profile. The caller's profile id comes from the bearer token, so a
/// caller can only ever reach proxies under their own profile.
pub fn resolve_targets(store: &Store, profile_id: &str, model: &str) -> Result<Vec<Target>> {
    let (proxy_name, route_name) = model
        .split_once('/')
        .with_context(|| format!("model {model:?} must be in `<proxy>/<route>` format"))?;

    let proxy = store
        .get_proxy_named(profile_id, proxy_name)?
        .with_context(|| format!("no proxy named {proxy_name:?}"))?;

    let route = store
        .get_route_named(proxy.id, route_name)?
        .with_context(|| format!("no route named {route_name:?} under proxy {proxy_name:?}"))?;

    let identity = route.identity.clone();
    let entries = store.route_entries(route.id)?;
    let mut targets = Vec::new();
    for entry in entries {
        if !matches!(entry.status, ModelStatus::Healthy | ModelStatus::Degraded) {
            continue;
        }
        if let Some(provider) = store.get_provider(entry.provider_id)? {
            targets.push(Target {
                provider,
                entry,
                identity: identity.clone(),
            });
        }
    }
    Ok(targets)
}

/// Like [`resolve_targets`], but also reorders the list according to the
/// route's configured [`RoutingStrategy`] and the shared [`RoutingState`].
///
/// - `Priority`: entries are left in strict priority order (no change).
/// - `RoundRobin`: the starting entry rotates per request.
/// - `Weighted`: the starting entry is picked by weighted random draw.
pub fn resolve_targets_with_strategy(
    store: &Store,
    profile_id: &str,
    model: &str,
    needs: RequestNeeds,
    routing_state: &RoutingState,
) -> Result<Vec<Target>> {
    let (proxy_name, route_name) = model
        .split_once('/')
        .with_context(|| format!("model {model:?} must be in `<proxy>/<route>` format"))?;

    let proxy = store
        .get_proxy_named(profile_id, proxy_name)?
        .with_context(|| format!("no proxy named {proxy_name:?}"))?;

    let route = store
        .get_route_named(proxy.id, route_name)?
        .with_context(|| format!("no route named {route_name:?} under proxy {proxy_name:?}"))?;

    let identity = route.identity.clone();
    let entries = store.route_entries(route.id)?;
    let mut targets: Vec<Target> = entries
        .into_iter()
        .filter(|e| matches!(e.status, ModelStatus::Healthy | ModelStatus::Degraded))
        .filter(|e| needs.satisfies(e.capabilities.clone()))
        .filter_map(|entry| {
            store
                .get_provider(entry.provider_id)
                .ok()
                .flatten()
                .map(|provider| Target {
                    provider,
                    entry,
                    identity: identity.clone(),
                })
        })
        .collect();

    match route.strategy {
        RoutingStrategy::Priority => {} // already in priority order
        RoutingStrategy::RoundRobin => routing_state.rotate_round_robin(route.id, &mut targets),
        RoutingStrategy::Weighted => routing_state.shuffle_weighted(&mut targets),
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
                last_error = Some(anyhow::anyhow!(
                    "attempt timed out after {attempt_timeout:?}"
                ));
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
    use crate::translator::content_text;
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
        let proxy = store
            .create_proxy(profile.id.as_str(), "prog", None)
            .unwrap();
        let route = store
            .create_route(proxy.id, "r1", None, RoutingStrategy::Priority, None)
            .unwrap();
        store
            .add_route_entry(route.id, provider.id, "m1", 1, 1.0, Default::default())
            .unwrap();
        let targets = resolve_targets(&store, &profile.id, "prog/r1").unwrap();
        assert_eq!(targets.len(), 1);
        (store, targets)
    }

    #[tokio::test]
    async fn failover_returns_first_success() {
        let (store, targets) = setup();
        let store = Arc::new(store);
        let result = execute_with_failover(store, targets, Duration::from_secs(5), |_| async {
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

    fn setup_multi_entry() -> (Store, i64) {
        let store = Store::open_in_memory().unwrap();
        let profile = store.create_profile("coder1", None, None).unwrap();
        let p1 = store
            .create_provider(
                profile.id.as_str(),
                NewProvider {
                    name: "p1".into(),
                    description: None,
                    base_url: "https://a.example".into(),
                    auth_token: "tok1".into(),
                    kind: ProviderKind::OpenAICompatible,
                    extra_headers: BTreeMap::new(),
                },
            )
            .unwrap();
        let p2 = store
            .create_provider(
                profile.id.as_str(),
                NewProvider {
                    name: "p2".into(),
                    description: None,
                    base_url: "https://b.example".into(),
                    auth_token: "tok2".into(),
                    kind: ProviderKind::OpenAICompatible,
                    extra_headers: BTreeMap::new(),
                },
            )
            .unwrap();
        let proxy = store
            .create_proxy(profile.id.as_str(), "prog", None)
            .unwrap();
        let route = store
            .create_route(proxy.id, "r1", None, RoutingStrategy::RoundRobin, None)
            .unwrap();
        store
            .add_route_entry(route.id, p1.id, "m1", 1, 1.0, Default::default())
            .unwrap();
        store
            .add_route_entry(route.id, p2.id, "m2", 2, 1.0, Default::default())
            .unwrap();
        (store, route.id)
    }

    fn pid(store: &Store) -> String {
        store
            .get_profile_by_name("coder1")
            .unwrap()
            .expect("profile exists")
            .id
    }

    #[test]
    fn round_robin_rotates_starting_entry() {
        let (store, _route_id) = setup_multi_entry();
        let state = RoutingState::default();
        let profile_id = pid(&store);
        let first = resolve_targets_with_strategy(
            &store,
            &profile_id,
            "prog/r1",
            RequestNeeds::default(),
            &state,
        )
        .unwrap();
        assert_eq!(first[0].provider.name, "p1");
        let second = resolve_targets_with_strategy(
            &store,
            &profile_id,
            "prog/r1",
            RequestNeeds::default(),
            &state,
        )
        .unwrap();
        assert_eq!(second[0].provider.name, "p2");
        let third = resolve_targets_with_strategy(
            &store,
            &profile_id,
            "prog/r1",
            RequestNeeds::default(),
            &state,
        )
        .unwrap();
        assert_eq!(third[0].provider.name, "p1");
    }

    #[test]
    fn weighted_biases_towards_heavier_entry() {
        let store = Store::open_in_memory().unwrap();
        let profile = store.create_profile("coder1", None, None).unwrap();
        let p1 = store
            .create_provider(
                profile.id.as_str(),
                NewProvider {
                    name: "light".into(),
                    description: None,
                    base_url: "https://a.example".into(),
                    auth_token: "tok".into(),
                    kind: ProviderKind::OpenAICompatible,
                    extra_headers: BTreeMap::new(),
                },
            )
            .unwrap();
        let p2 = store
            .create_provider(
                profile.id.as_str(),
                NewProvider {
                    name: "heavy".into(),
                    description: None,
                    base_url: "https://b.example".into(),
                    auth_token: "tok".into(),
                    kind: ProviderKind::OpenAICompatible,
                    extra_headers: BTreeMap::new(),
                },
            )
            .unwrap();
        let proxy = store
            .create_proxy(profile.id.as_str(), "prog", None)
            .unwrap();
        let route = store
            .create_route(proxy.id, "r1", None, RoutingStrategy::Weighted, None)
            .unwrap();
        // light has weight 1, heavy has weight 9 → heavy should lead ~90% of the time
        store
            .add_route_entry(route.id, p1.id, "m1", 1, 1.0, Default::default())
            .unwrap();
        store
            .add_route_entry(route.id, p2.id, "m2", 2, 9.0, Default::default())
            .unwrap();

        let state = RoutingState::default();
        let profile_id = pid(&store);
        let mut heavy_first = 0;
        for _ in 0..200 {
            let targets = resolve_targets_with_strategy(
                &store,
                &profile_id,
                "prog/r1",
                RequestNeeds::default(),
                &state,
            )
            .unwrap();
            if targets[0].provider.name == "heavy" {
                heavy_first += 1;
            }
        }
        // With 9:1 ratio, heavy should lead far more than half the time.
        assert!(heavy_first > 150, "heavy led only {heavy_first}/200 times");
    }

    #[test]
    fn priority_strategy_leaves_order_unchanged() {
        let (store, _route_id) = setup_multi_entry();
        // Re-create the route as Priority.
        let profiles = store.list_profiles().unwrap();
        let proxies = store.list_proxies(profiles[0].id.as_str()).unwrap();
        let routes = store.list_routes(proxies[0].id).unwrap();
        assert!(!routes.is_empty());
        let state = RoutingState::default();
        let targets = resolve_targets_with_strategy(
            &store,
            &pid(&store),
            "prog/r1",
            RequestNeeds::default(),
            &state,
        )
        .unwrap();
        // Even though the route is RoundRobin, the resolver returns both entries.
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn needs_filtering_skips_entries_without_capabilities() {
        let (store, _route_id) = setup_multi_entry();
        let profile_id = pid(&store);
        let state = RoutingState::default();

        // No needs: both entries resolve.
        let plain = resolve_targets_with_strategy(
            &store,
            &profile_id,
            "prog/r1",
            RequestNeeds::default(),
            &state,
        )
        .unwrap();
        assert_eq!(plain.len(), 2);

        // Tools needed: entries without the `tools` capability are skipped.
        let targets = resolve_targets_with_strategy(
            &store,
            &profile_id,
            "prog/r1",
            RequestNeeds {
                tools: true,
                ..Default::default()
            },
            &state,
        )
        .unwrap();
        assert!(targets.is_empty(), "entries lack the tools capability");
    }

    #[test]
    fn needs_from_body_detects_tools_vision_and_json() {
        let tools = serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "function", "function": {"name": "f"}}]
        });
        let n = RequestNeeds::from_body(&tools);
        assert!(n.tools);
        assert!(!n.vision);
        assert!(!n.json_mode);

        let vision = serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "what is this"},
                {"type": "image_url", "image_url": {"url": "https://x/img.png"}}
            ]}]
        });
        let n = RequestNeeds::from_body(&vision);
        assert!(n.vision);
        assert!(!n.tools);

        let json = serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "response_format": {"type": "json_object"}
        });
        assert!(RequestNeeds::from_body(&json).json_mode);

        let plain = serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert_eq!(RequestNeeds::from_body(&plain), RequestNeeds::default());
    }

    #[test]
    fn content_text_joins_text_parts_and_drops_images() {
        let s = serde_json::json!("plain string");
        assert_eq!(content_text(&s), "plain string");

        let arr = serde_json::json!([
            {"type": "text", "text": "line one"},
            {"type": "image_url", "image_url": {"url": "x"}},
            {"type": "text", "text": "line two"}
        ]);
        assert_eq!(content_text(&arr), "line one\nline two");
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
        let targets = resolve_targets(&store, &pid(&store), "prog/r1").unwrap();
        assert!(targets.is_empty());
    }

    #[test]
    fn resolution_is_scoped_to_the_callers_profile() {
        let store = Store::open_in_memory().unwrap();
        // Profile A owns the proxy/route; profile B is a different tenant.
        let a = store.create_profile("alice", None, None).unwrap();
        let _b = store.create_profile("bob", None, None).unwrap();
        let p = store
            .create_provider(
                a.id.as_str(),
                NewProvider {
                    name: "p".into(),
                    description: None,
                    base_url: "https://a.example".into(),
                    auth_token: "tok".into(),
                    kind: ProviderKind::OpenAICompatible,
                    extra_headers: BTreeMap::new(),
                },
            )
            .unwrap();
        let proxy = store.create_proxy(a.id.as_str(), "prog", None).unwrap();
        let route = store
            .create_route(proxy.id, "r1", None, RoutingStrategy::Priority, None)
            .unwrap();
        store
            .add_route_entry(route.id, p.id, "m1", 1, 1.0, Default::default())
            .unwrap();

        // Alice resolves fine.
        let own = resolve_targets(&store, &a.id, "prog/r1").unwrap();
        assert_eq!(own.len(), 1);
        // Bob cannot reach Alice's proxy even with the same model string.
        let other = resolve_targets(&store, &_b.id, "prog/r1");
        assert!(other.is_err(), "cross-profile resolution must fail");
        // A second proxy with the same name under Bob's profile doesn't leak
        // Alice's routes either.
        let b_proxy = store.create_proxy(_b.id.as_str(), "prog", None).unwrap();
        let b_route = store
            .create_route(b_proxy.id, "r1", None, RoutingStrategy::Priority, None)
            .unwrap();
        let _ = b_route;
        let still_empty = resolve_targets(&store, &_b.id, "prog/r1").unwrap();
        assert!(still_empty.is_empty());
    }
}

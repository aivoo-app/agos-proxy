//! Request routing and priority failover.
//!
//! Given a model string (e.g. `programmer/php-developer-3.5-flash`), resolve the
//! route and try each healthy entry in priority order until one succeeds.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context as _, Result};
use rand::Rng;
use tokio::time::{timeout, Duration};

use crate::domain::{ModelStatus, PromptCachePolicy, Provider, RouteEntry, RoutingStrategy};
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
    /// Infer the needs from a raw OpenAI request body.
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
    pub fn satisfies(&self, caps: &crate::domain::RouteCapabilities) -> bool {
        (!self.tools || caps.tools)
            && (!self.vision || caps.vision)
            && (!self.json_mode || caps.json_mode)
    }

    /// Whether this request needs nothing beyond a plain text chat completion.
    pub fn is_empty(&self) -> bool {
        !self.tools && !self.vision && !self.json_mode
    }

    /// The needed capabilities as stable machine-readable labels, for error
    /// bodies and logs.
    pub fn as_labels(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.tools {
            out.push("tools");
        }
        if self.vision {
            out.push("vision");
        }
        if self.json_mode {
            out.push("json_mode");
        }
        out
    }

    /// Human-readable list of what this request needs, for an error message.
    pub fn missing_list(&self) -> String {
        let labels = self.as_labels();
        if labels.is_empty() {
            return "none".to_string();
        }
        labels.join(", ")
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
    /// Prompt-cache policy of the *outermost* route the caller named. Nested
    /// routes inherit it rather than overriding it: the caller's choice should
    /// govern the whole chain, and a shared foreign route must not silently
    /// change caching behavior for the profiles that reference it.
    pub prompt_cache: PromptCachePolicy,
}

/// Resolve a model string to an ordered list of healthy targets, scoped to the
/// given profile. The caller's profile id comes from the bearer token, so a
/// caller can only ever reach proxies under their own profile — plus shared
/// resources other profiles have published.
pub fn resolve_targets(store: &Store, profile_id: &str, model: &str) -> Result<Vec<Target>> {
    let route = resolve_route(store, profile_id, model)?;
    let mut targets = expand_route_targets(store, profile_id, route.id, RequestNeeds::default(), 0)?;
    for target in &mut targets {
        target.identity = route.identity.clone();
        target.prompt_cache = route.prompt_cache;
    }
    Ok(warm_first(
        targets,
        |t| t.entry.cooldown_until,
        now_millis(),
    ))
}

/// Look up the proxy + route a model string names, enforcing profile scope.
fn resolve_route(store: &Store, profile_id: &str, model: &str) -> Result<crate::domain::Route> {
    let (proxy_name, route_name) = model
        .split_once('/')
        .with_context(|| format!("model {model:?} must be in `<proxy>/<route>` format"))?;

    let proxy = store
        .get_proxy_named(profile_id, proxy_name)?
        .with_context(|| format!("no proxy named {proxy_name:?}"))?;

    store
        .get_route_named(proxy.id, route_name)?
        .with_context(|| format!("no route named {route_name:?} under proxy {proxy_name:?}"))
}

/// How deep a route may reference other routes before the proxy refuses.
const MAX_ROUTE_DEPTH: usize = 8;

/// Expand a route's entries into concrete leaf targets.
///
/// Direct entries contribute themselves; nested entries (route-as-model)
/// recurse into the referenced route and splice its leaf targets in at that
/// position. Every hop re-checks sharing: a provider may only be used when the
/// caller owns it or it is shared, and a foreign route may only be entered when
/// it is shared. Depth and the DFS `visited` path bound nesting so a circular
/// reference fails the request instead of looping forever.
fn expand_route_targets(
    store: &Store,
    caller_profile: &str,
    route_id: i64,
    needs: RequestNeeds,
    depth: usize,
) -> Result<Vec<Target>> {
    expand_inner(store, caller_profile, route_id, needs, depth, &mut Vec::new())
}

fn expand_inner(
    store: &Store,
    caller_profile: &str,
    route_id: i64,
    needs: RequestNeeds,
    depth: usize,
    visited: &mut Vec<i64>,
) -> Result<Vec<Target>> {
    anyhow::ensure!(
        depth < MAX_ROUTE_DEPTH,
        "route chain nests deeper than {MAX_ROUTE_DEPTH} levels; refusing to expand"
    );
    anyhow::ensure!(
        !visited.contains(&route_id),
        "circular route reference: route {route_id} appears in its own chain"
    );
    visited.push(route_id);

    let mut targets = Vec::new();
    for entry in store.route_entries(route_id)? {
        if !matches!(entry.status, ModelStatus::Healthy | ModelStatus::Degraded) {
            continue;
        }
        if !needs.satisfies(&entry.capabilities) {
            continue;
        }
        if let Some(provider_id) = entry.provider_id {
            let provider = store
                .get_provider(provider_id)?
                .with_context(|| format!("provider {provider_id} missing"))?;
            anyhow::ensure!(
                provider.profile_id == caller_profile || provider.shared,
                "provider {:?} belongs to another profile and is not shared",
                provider.name
            );
            targets.push(Target {
                provider,
                entry,
                identity: None,
                // Overwritten by the caller-facing resolution entry point with
                // the outermost route's policy; nested routes never leak theirs.
                prompt_cache: PromptCachePolicy::default(),
            });
        } else if let Some(target_route_id) = entry.target_route_id {
            let owner = store
                .route_owner_profile(target_route_id)?
                .with_context(|| format!("route {target_route_id} missing"))?;
            let shared = store
                .get_route_by_id(target_route_id)?
                .map(|r| r.shared)
                .unwrap_or(false);
            anyhow::ensure!(
                shared || owner == caller_profile,
                "route {target_route_id} belongs to another profile and is not shared"
            );
            targets.extend(expand_inner(
                store,
                caller_profile,
                target_route_id,
                needs,
                depth + 1,
                visited,
            )?);
        }
    }

    visited.pop();
    Ok(targets)
}

/// Like [`resolve_targets`], but also reorders the list according to the
/// route's configured [`RoutingStrategy`] and the shared [`RoutingState`].
///
/// - `Priority`: entries are left in strict priority order (no change).
/// - `RoundRobin`: the starting entry rotates per request.
/// - `Weighted`: the starting entry is picked by weighted random draw.
/// - `Economy`: entries sorted cheapest-first by `price_per_1m` (unknown last),
///   so simple prompts never touch the flagship. Failover still escalates.
pub fn resolve_targets_with_strategy(
    store: &Store,
    profile_id: &str,
    model: &str,
    needs: RequestNeeds,
    routing_state: &RoutingState,
) -> Result<Vec<Target>> {
    let route = resolve_route(store, profile_id, model)?;

    // Identity is a caller-facing property: the outermost route wins, so a
    // nested route's own identity is ignored when it is used as a model.
    let identity = route.identity.clone();
    let mut targets = expand_route_targets(store, profile_id, route.id, needs, 0)?;
    for target in &mut targets {
        target.identity = identity.clone();
    }

    // Entries parked by an upstream rate limit sit out while any warm entry
    // remains, so a single exhausted key cannot absorb the round-robin turns
    // that a fresh key should be getting.
    let mut targets = warm_first(targets, |t| t.entry.cooldown_until, now_millis());

    match route.strategy {
        RoutingStrategy::Priority => {} // already in priority order
        RoutingStrategy::RoundRobin => routing_state.rotate_round_robin(route.id, &mut targets),
        RoutingStrategy::Weighted => routing_state.shuffle_weighted(&mut targets),
        RoutingStrategy::Economy => {
            // Cheap-first; unknown price (0.0) sinks to the end.
            targets.sort_by(|a, b| {
                let pa = if a.entry.price_per_1m <= 0.0 {
                    f64::INFINITY
                } else {
                    a.entry.price_per_1m
                };
                let pb = if b.entry.price_per_1m <= 0.0 {
                    f64::INFINITY
                } else {
                    b.entry.price_per_1m
                };
                pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
            });
        }
    }

    Ok(targets)
}

/// Whether an explicit escalation was requested (`X-Economy-Escalate: true`
/// header or `{"economy_escalate": true}` body flag). Callers use this to
/// force the flagship when they know the task is hard.
///
/// `body` is the raw request JSON (unknown keys live at the top level).
/// [`wants_escalation_header`] covers the header variant; combine both at the
/// call site.
pub fn wants_escalation(body: &serde_json::Value) -> bool {
    // Top-level flag (OpenAI surface). `ChatRequest::extra` flattens unknown
    // keys, so also look one level inside `extra` for callers that pass the
    // already-parsed canonical value.
    if body
        .get("economy_escalate")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    body.get("extra")
        .and_then(|e| e.get("economy_escalate"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Header variant of [`wants_escalation`]: `X-Economy-Escalate: true` / `1`.
pub fn wants_escalation_header(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("true") | Some("1") | Some("yes")
    )
}

/// How long a rate-limited entry is parked when the upstream does not say,
/// in seconds. Overridable with `AGOS_RATE_LIMIT_COOLDOWN_SECS`.
pub fn rate_limit_cooldown_secs() -> i64 {
    std::env::var("AGOS_RATE_LIMIT_COOLDOWN_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(60)
}

/// Wall-clock milliseconds, the same unit the store persists.
fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Order candidates so that entries which are *not* cooling come first.
///
/// When every candidate is cooling, the cooling ones are returned in
/// soonest-to-warm order instead. That fallback matters: a request must never
/// hard-fail merely because every key is currently throttled — it should still
/// take the best available shot, and the try succeeds the moment a window
/// opens.
fn warm_first<T>(mut items: Vec<T>, cooldown_until: impl Fn(&T) -> i64, now: i64) -> Vec<T> {
    let (warm, mut cooling): (Vec<T>, Vec<T>) = items
        .drain(..)
        .partition(|item| cooldown_until(item) <= now);
    if warm.is_empty() {
        cooling.sort_by_key(|item| cooldown_until(item));
        cooling
    } else {
        warm
    }
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
    // (target, upstream status code if the attempt carried one, upstream error
    // message, retry hint). The first three drive the demotion classification
    // below; the hint drives the cooldown applied to a rate-limited entry.
    let mut failures: Vec<(Target, Option<i64>, String, Option<i64>)> = Vec::new();
    for target in &targets {
        match timeout(attempt_timeout, attempt_fn(target.clone())).await {
            Ok(Ok(bytes)) => return Ok(bytes),
            Ok(Err(e)) => {
                let provider_err = e.downcast_ref::<crate::adapter::outbound::ProviderError>();
                let status = provider_err.map(|pe| pe.status.as_u16() as i64);
                let retry_after = provider_err.and_then(|pe| pe.retry_after);
                // anyhow's Display only shows the outer context, so the raw
                // upstream body (which names images on a rejection) must come
                // from the downcast. Transport failures fall back to the error
                // string.
                let message = provider_err
                    .map(|pe| pe.body.clone())
                    .unwrap_or_else(|| e.to_string());
                last_error = Some(e);
                failures.push((target.clone(), status, message, retry_after));
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
                failures.push((target.clone(), None, "upstream timeout".to_string(), None));
                tracing::warn!(
                    provider = %target.provider.name,
                    model = %target.entry.model_id,
                    "attempt timed out, failing over"
                );
            }
        }
    }

    // Classify each failure (see [`classify_failure`]) and apply the matching
    // action. The three outcomes are deliberately distinct: provider-side
    // failures take the entry out of rotation, an image refusal only clears that
    // entry's `vision` flag (it still serves text — parking it would waste a
    // working key), and request-side/mask failures leave health untouched.
    for (target, status_code, message, retry_after) in &failures {
        apply_failure_action(&store, target, *status_code, message);
        // A quota refusal needs more than `Degraded`: degraded entries stay
        // selectable, so the entry is explicitly parked until the upstream's
        // window (or our fallback) expires. This is what keeps several keys
        // useful rather than re-picking the one that is already exhausted.
        if *status_code == Some(429) {
            let secs = retry_after.unwrap_or_else(rate_limit_cooldown_secs);
            let until = now_millis() + secs.max(0) * 1000;
            let _ = store.set_route_entry_cooldown(target.entry.id, until);
        }
    }

    match last_error {
        Some(e) => Err(e),
        None => bail!("all targets failed"),
    }
}

/// What to do with the route entry that produced a failure.
///
/// Three outcomes, deliberately distinguished, because conflating them is what
/// used to take a perfectly good key out of rotation whenever a client sent an
/// image to a model that does not accept images.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureAction {
    /// The upstream refused *this kind of request* (images), not the key. Clear
    /// the entry's `vision` flag and leave its health alone: it keeps serving
    /// text, tools and JSON exactly as before, and future image requests skip it
    /// instead of rediscovering the refusal on every call.
    ClearVision,
    /// Provider-side failure: take the entry out of rotation.
    Demote(ModelStatus),
    /// Request-side failure, or a resource-side skip that says nothing about the
    /// entry's health. Leave it untouched and fail over.
    Ignore,
}

/// Classify a failure into a [`FailureAction`].
///
/// Provider-side failures demote the entry (429 only `Degraded`, everything else
/// `Unhealthy`). Client-side 4xx that originates from the translated request body
/// itself never demotes — except a 4xx whose error message refuses images, which
/// clears the entry's vision flag rather than demoting it. Local adapter
/// capability rejections (tools/vision/JSON/stream unsupported by the adapter,
/// marked with `ADAPTER_CAPABILITY_SKIP`) and mask failures are ignored: the
/// request needs a different entry, not a sicker one. Failures without a known
/// status (transport errors, timeouts) are treated as provider-side.
pub(crate) fn classify_failure(status_code: Option<i64>, message: &str) -> FailureAction {
    // A mask that refused the request, or one that cannot carry this payload,
    // says nothing about the provider's health.
    if message.contains(crate::mask::MASK_SKIP) || message.contains(crate::mask::MASK_FAILED) {
        return FailureAction::Ignore;
    }
    if message.contains(crate::adapter::outbound::responses::ADAPTER_CAPABILITY_SKIP) {
        return FailureAction::Ignore;
    }
    match status_code {
        Some(429) => FailureAction::Demote(ModelStatus::Degraded),
        Some(c) if (400..500).contains(&c) => {
            if image_rejection(message) {
                FailureAction::ClearVision
            } else {
                FailureAction::Ignore
            }
        }
        _ => FailureAction::Demote(ModelStatus::Unhealthy),
    }
}

/// Classify a failure and apply the resulting action to `target`'s entry.
///
/// Shared by the buffered failover loop and the streaming path so both surfaces
/// treat an image refusal the same way: clear `vision`, never park the key.
pub fn apply_failure_action(
    store: &Store,
    target: &Target,
    status_code: Option<i64>,
    message: &str,
) -> FailureAction {
    let action = classify_failure(status_code, message);
    match action {
        FailureAction::ClearVision => {
            if target.entry.capabilities.vision {
                let _ = store.set_route_entry_vision(target.entry.id, false);
                tracing::info!(
                    provider = %target.provider.name,
                    model = %target.entry.model_id,
                    "upstream rejects image input; cleared vision capability for this entry"
                );
            }
        }
        FailureAction::Demote(status) => {
            let _ = store.set_route_entry_status(target.entry.id, status);
        }
        FailureAction::Ignore => {}
    }
    action
}

/// Classify a failure status into a demotion decision, ignoring the
/// vision-specific outcome. Retained for callers that only care about health.
#[cfg(test)]
pub(crate) fn demote_status_for(status_code: Option<i64>, message: &str) -> Option<ModelStatus> {
    match classify_failure(status_code, message) {
        FailureAction::Demote(status) => Some(status),
        FailureAction::ClearVision | FailureAction::Ignore => None,
    }
}

/// Recognize explicit capability refusals, not arbitrary mentions of images.
/// For JSON envelopes, inspect only the dedicated error field, never request
/// echoes.
fn image_rejection(message: &str) -> bool {
    let Some(text) = error_text(message) else {
        return false;
    };
    let lower = text.to_ascii_lowercase();
    let normalized = lower.split_whitespace().collect::<Vec<_>>().join(" ");

    // A rejection aimed at one file's encoding, size or transfer is a property
    // of the payload, not a missing model capability: the upstream may accept
    // the next image just fine, and non-image traffic must not be affected.
    const PAYLOAD_SPECIFIC: [&str; 12] = [
        "media type",
        "mime",
        "file format",
        "image format",
        "vision format",
        "too large",
        "corrupt",
        "decode",
        "download",
        "dimensions",
        "resolution",
        "aspect ratio",
    ];
    if PAYLOAD_SPECIFIC
        .iter()
        .any(|qualifier| normalized.contains(qualifier))
    {
        return false;
    }

    // Strong signal: "image input" and friends name the modality capability
    // itself. A request echo carries `image_url`, never "image input", and we
    // already read only the dedicated error field, so any refusal wording in
    // the same message is enough. This catches Gemini's
    // "Unable to process the provided image input".
    const INPUT_TOKENS: [&str; 4] = [
        "image input",
        "image inputs",
        "vision input",
        "multimodal input",
    ];
    const REFUSALS_ANYWHERE: [&str; 11] = [
        "not supported",
        "unsupported",
        "not able to",
        "unable to",
        "cannot",
        "can't",
        "does not support",
        "do not support",
        "doesn't support",
        "does not accept",
        "doesn't accept",
    ];
    if INPUT_TOKENS
        .iter()
        .any(|token| contains_phrase(&normalized, token))
        && REFUSALS_ANYWHERE
            .iter()
            .any(|refusal| normalized.contains(refusal))
    {
        return true;
    }

    // Precise pairs, for phrasings that never say "input" (e.g. "images are
    // not supported", "this model does not support images").
    const REFUSALS: [&str; 5] = [
        "does not support",
        "do not support",
        "doesn't support",
        "does not accept",
        "doesn't accept",
    ];
    const MODALITIES: [&str; 5] = ["image", "images", "vision", "vision inputs", "multimodal"];
    MODALITIES.iter().any(|modality| {
        REFUSALS
            .iter()
            .any(|refusal| contains_phrase(&normalized, &format!("{refusal} {modality}")))
            || [
                "not supported",
                "is not supported",
                "are not supported",
                "is unsupported",
                "are unsupported",
            ]
            .iter()
            .any(|suffix| contains_phrase(&normalized, &format!("{modality} {suffix}")))
    })
}

/// Pull the human-readable error out of an upstream body. Only dedicated error
/// fields are read — never arbitrary request-echo fields — and `message` may be
/// a plain string, an array of `{"type":"text","text":…}` blocks, or an object
/// carrying `text`. Bodies that are already plain text or a bare JSON string
/// are used as-is.
fn error_text(body: &str) -> Option<String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Some(body.to_string());
    };
    match &parsed {
        serde_json::Value::String(s) => Some(s.clone()),
        _ => ["/error/message", "/error", "/message", "/detail"]
            .iter()
            .find_map(|pointer| parsed.pointer(pointer).and_then(value_text)),
    }
}

/// Extract text from a value that may be a string, an array of content blocks,
/// or an object carrying `text`.
fn value_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(map) => map
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        serde_json::Value::Array(items) => {
            let parts: Vec<&str> = items.iter().filter_map(value_text_ref).collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(" "))
            }
        }
        _ => None,
    }
}

fn value_text_ref(value: &serde_json::Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("text").and_then(serde_json::Value::as_str))
}

/// Do not match modality words inside model names or request-field names.
fn contains_phrase(message: &str, phrase: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-');
    message.match_indices(phrase).any(|(start, _)| {
        !message[..start].ends_with(is_word)
            && !message[start + phrase.len()..].starts_with(is_word)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ProviderKind, RouteCapabilities, RoutingStrategy};
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
                    kind: ProviderKind::OpenAI,
                    extra_headers: BTreeMap::new(),
                    masking_server_id: None,
                    shared: false,
                },
            )
            .unwrap();
        let proxy = store
            .create_proxy(profile.id.as_str(), "prog", None)
            .unwrap();
        let route = store
            .create_route(proxy.id, "r1", None, RoutingStrategy::Priority, None)
            .unwrap();
        // Capabilities as the CLI/bootstrap would infer them, so this fixture
        // exercises the same entries an operator would actually create.
        store
            .add_route_entry(
                route.id,
                provider.id,
                "m1",
                1,
                1.0,
                RouteCapabilities::infer_from_model_id("m1"),
            )
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
                    kind: ProviderKind::OpenAI,
                    extra_headers: BTreeMap::new(),
                    masking_server_id: None,
                    shared: false,
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
                    kind: ProviderKind::OpenAI,
                    extra_headers: BTreeMap::new(),
                    masking_server_id: None,
                    shared: false,
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
                    kind: ProviderKind::OpenAI,
                    extra_headers: BTreeMap::new(),
                    masking_server_id: None,
                    shared: false,
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
                    kind: ProviderKind::OpenAI,
                    extra_headers: BTreeMap::new(),
                    masking_server_id: None,
                    shared: false,
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
    fn economy_sorts_cheapest_first() {
        let store = Store::open_in_memory().unwrap();
        let profile = store.create_profile("coder1", None, None).unwrap();
        let mk = |name: &str| {
            store
                .create_provider(
                    profile.id.as_str(),
                    NewProvider {
                        name: name.into(),
                        description: None,
                        base_url: "https://a.example".into(),
                        auth_token: "tok".into(),
                        kind: ProviderKind::OpenAI,
                        extra_headers: BTreeMap::new(),
                        masking_server_id: None,
                        shared: false,
                    },
                )
                .unwrap()
        };
        let cheap_p = mk("cheap-p");
        let flagship_p = mk("flag-p");
        let proxy = store
            .create_proxy(profile.id.as_str(), "prog", None)
            .unwrap();
        let route = store
            .create_route(proxy.id, "r1", None, RoutingStrategy::Economy, None)
            .unwrap();
        // Insert expensive first — Economy must still try cheap first.
        let exp = store
            .add_route_entry(
                route.id,
                flagship_p.id,
                "provider-pro",
                1,
                1.0,
                Default::default(),
            )
            .unwrap();
        let chp = store
            .add_route_entry(
                route.id,
                cheap_p.id,
                "provider-mini",
                2,
                1.0,
                Default::default(),
            )
            .unwrap();
        store.set_route_entry_price(exp.id, 6.0).unwrap();
        store.set_route_entry_price(chp.id, 0.4).unwrap();

        let state = RoutingState::default();
        let targets = resolve_targets_with_strategy(
            &store,
            &profile.id,
            "prog/r1",
            RequestNeeds::default(),
            &state,
        )
        .unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].entry.model_id, "provider-mini");
        assert_eq!(targets[1].entry.model_id, "provider-pro");
    }

    #[test]
    fn escalation_flag_is_detected() {
        assert!(!wants_escalation(&serde_json::json!({})));
        assert!(wants_escalation(
            &serde_json::json!({"economy_escalate": true})
        ));
        assert!(!wants_escalation(
            &serde_json::json!({"economy_escalate": false})
        ));
        // Canonical (ChatRequest-serialized) shape nests unknown keys under `extra`.
        assert!(wants_escalation(
            &serde_json::json!({"extra": {"economy_escalate": true}})
        ));
        assert!(!wants_escalation_header(None));
        assert!(wants_escalation_header(Some("true")));
        assert!(wants_escalation_header(Some("1")));
        assert!(!wants_escalation_header(Some("false")));
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

    #[test]
    fn image_rejection_ignores_unrelated_errors_and_echoes() {
        for message in [
            "unsupported parameter max_tokens; image_url=x",
            "cannot decode image: corrupt data",
            "unable to download image URL",
            "unsupported image format: use PNG",
            "model vision-pro was not found",
            "this model does not support image_url.detail",
            "this model does not support vision-pro",
            "revision is not supported",
            "錯誤： image too large",
            r#"{"error":{"message":"unsupported parameter"},"request":{"text":"image input not supported","image_url":"x"}}"#,
            r#"{"request":{"message":"image input not supported"}}"#,
            r#"{"error":{"message":null},"request":"image input not supported"}"#,
            "",
        ] {
            assert_eq!(demote_status_for(Some(400), message), None, "{message}");
            assert_eq!(demote_status_for(Some(422), message), None, "{message}");
        }
    }

    #[test]
    fn image_rejection_recognizes_explicit_capability_refusals() {
        for message in [
            "image input not supported",
            "Images are not supported by this model.",
            "This model does not support IMAGE input.",
            "This model doesn't accept images.",
            "vision is unsupported",
            "multimodal input is not supported",
            "錯誤： images are not supported",
            "image  input\nnot supported",
            r#"{"error":{"message":"image input not supported"}}"#,
            r#"{"error":"This model does not accept images"}"#,
            r#"{"message":"vision is not supported"}"#,
        ] {
            assert_eq!(
                classify_failure(Some(400), message),
                FailureAction::ClearVision,
                "{message}"
            );
        }
    }

    #[test]
    fn image_rejection_handles_real_upstream_error_envelopes() {
        // Anthropic: {"type":"error","error":{"type":…,"message":…}}
        assert_eq!(
            classify_failure(
                Some(400),
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"This model does not support image input."}}"#
            ),
            FailureAction::ClearVision
        );
        // Google/Gemini: refusal wording is "unable to process".
        assert_eq!(
            classify_failure(
                Some(400),
                r#"{"error":{"code":400,"message":"Unable to process the provided image input.","status":"INVALID_ARGUMENT"}}"#
            ),
            FailureAction::ClearVision
        );
        // `message` as an array of content blocks.
        assert_eq!(
            classify_failure(
                Some(400),
                r#"{"error":{"message":[{"type":"text","text":"images are not supported"}]}}"#
            ),
            FailureAction::ClearVision
        );
        // A bare JSON string body.
        assert_eq!(
            classify_failure(Some(400), r#""images are not supported""#),
            FailureAction::ClearVision
        );
        // Non-standard top-level `detail` field.
        assert_eq!(
            classify_failure(Some(400), r#"{"detail":"images are not supported"}"#),
            FailureAction::ClearVision
        );
        // An array with no extractable text is not evidence of a capability gap.
        assert_eq!(
            demote_status_for(Some(400), r#"{"error":{"message":[{"foo":"bar"}]}}"#),
            None
        );
    }

    #[test]
    fn payload_specific_image_errors_do_not_demote() {
        // The upstream can take images; this particular file is the problem.
        for message in [
            "unsupported media type for vision input",
            "unsupported image format: use PNG",
            "image dimensions exceed the maximum resolution",
            "Unsupported MIME type: image/webp",
            "unable to decode image data",
            "request payload too large",
        ] {
            assert_eq!(demote_status_for(Some(415), message), None, "{message}");
            assert_eq!(demote_status_for(Some(400), message), None, "{message}");
        }
    }

    #[test]
    fn image_rejecting_4xx_clears_vision_but_plain_4xx_does_not() {
        // Explicit capability refusals clear the entry vision flag, not its health.
        assert_eq!(
            classify_failure(
                Some(400),
                r#"{"error":{"message":"image input not supported"}}"#
            ),
            FailureAction::ClearVision
        );
        assert_eq!(
            demote_status_for(Some(415), "unsupported media type for vision input"),
            None
        );
        // Matching is ASCII-case-insensitive.
        assert_eq!(
            classify_failure(Some(404), "Model does not support IMAGE input"),
            FailureAction::ClearVision
        );
        // A refusal phrase and the image term must actually pair up: a 4xx that
        // echoes the request payload (whose parts carry `image_url` keys) next
        // to an unrelated error must not demote a healthy upstream.
        assert_eq!(
            demote_status_for(Some(400), "invalid parameter: max_tokens"),
            None
        );
        assert_eq!(
            demote_status_for(Some(422), "request body exceeds the context window"),
            None
        );
        // A request echo is not a capability rejection.
        assert_eq!(
            demote_status_for(
                Some(400),
                "cannot process the request: the upstream rejected it. request echo: \
                 {\"messages\":[{\"content\":[{\"type\":\"image_url\"}]}]}"
            ),
            None
        );
        // A mere mention without a refusal phrase is not a rejection either.
        assert_eq!(
            demote_status_for(Some(404), "model vision-pro was not found"),
            None
        );
        // A processing failure does not prove missing modality support.
        assert_eq!(demote_status_for(Some(400), "cannot process images"), None);

        // Everything else about the old classification is unchanged.
        assert_eq!(demote_status_for(Some(400), "invalid temperature"), None);
        assert_eq!(demote_status_for(Some(422), "request too large"), None);
        // Local adapter capability rejections fail over without demoting: the
        // request needs a different entry, not a sicker one.
        assert_eq!(
            demote_status_for(
                None,
                "adapter capability skip: tools/function calling not supported \
                 by openai_responses adapter; use a tools-capable upstream"
            ),
            None
        );
        assert_eq!(
            demote_status_for(
                Some(500),
                "adapter capability skip: image (vision) input not supported"
            ),
            None
        );
        assert_eq!(
            demote_status_for(Some(429), "rate limited"),
            Some(ModelStatus::Degraded)
        );
        assert_eq!(
            demote_status_for(Some(500), "boom"),
            Some(ModelStatus::Unhealthy)
        );
        assert_eq!(
            demote_status_for(None, "transport failure"),
            Some(ModelStatus::Unhealthy)
        );
        // A mask that refused the request, or one that cannot carry this
        // payload, never demotes the provider: the key is fine, the hop is not.
        // Without this, a single bad hop would take every key behind it dark.
        assert_eq!(
            demote_status_for(
                Some(502),
                &format!(
                    "{} egress mask \"edge-1\" returned 502",
                    crate::mask::MASK_FAILED
                )
            ),
            None
        );
        assert_eq!(
            demote_status_for(
                Some(413),
                &format!("{} body too large for the hop", crate::mask::MASK_SKIP)
            ),
            None
        );
    }

    #[test]
    fn cooling_entries_are_skipped_while_warm_ones_remain() {
        // The other four keys are parked; only the warm one should be offered,
        // so round-robin stops handing turns to an exhausted key.
        let now = 1_000_000;
        let entries = vec![(1i64, 0i64), (2, now + 5_000), (3, now + 1_000)];
        let ordered = warm_first(entries, |e| e.1, now);
        assert_eq!(ordered, vec![(1, 0)]);
    }

    #[test]
    fn when_every_entry_is_cooling_the_soonest_to_warm_is_tried_first() {
        // All five keys throttled must not mean "fail the request": the closest
        // window is tried, and the request succeeds as soon as one opens.
        let now = 1_000_000;
        let entries = vec![(1i64, now + 9_000), (2, now + 1_000), (3, now + 5_000)];
        let ordered = warm_first(entries, |e| e.1, now);
        assert_eq!(
            ordered.iter().map(|e| e.0).collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
    }

    #[tokio::test]
    async fn a_429_parks_the_entry_for_the_window_the_upstream_asked_for() {
        let (store, targets) = setup();
        let entry_id = targets[0].entry.id;
        let route_id = targets[0].entry.route_id;
        let store = Arc::new(store);

        let result =
            execute_with_failover(store.clone(), targets, Duration::from_secs(5), |_| async {
                Err(anyhow::Error::new(
                    crate::adapter::outbound::ProviderError {
                        status: reqwest::StatusCode::TOO_MANY_REQUESTS,
                        body: "quota exhausted".to_string(),
                        retry_after: Some(30),
                    },
                ))
            })
            .await;
        assert!(result.is_err(), "a 429 must still surface to the caller");

        let entries = store.route_entries(route_id).unwrap();
        let entry = entries.iter().find(|e| e.id == entry_id).unwrap();
        // Health still degrades (visibility), but the entry is also parked.
        assert_eq!(entry.status, ModelStatus::Degraded);
        let now = chrono::Utc::now().timestamp_millis();
        assert!(
            entry.cooldown_until > now + 25_000 && entry.cooldown_until <= now + 31_000,
            "cool-down should follow the upstream's 30s hint, got {}",
            entry.cooldown_until
        );

        // And the single entry is still offered rather than hard-failing.
        let resolved =
            resolve_targets(&store, &store.list_profiles().unwrap()[0].id, "prog/r1").unwrap();
        assert_eq!(resolved.len(), 1, "a cooling entry is still a last resort");
    }

    #[test]
    fn the_default_cool_down_is_configurable() {
        // Read the documented default without mutating the process environment
        // for other tests: the fallback is what matters when an upstream gives
        // no hint at all.
        let previous = std::env::var("AGOS_RATE_LIMIT_COOLDOWN_SECS").ok();
        std::env::set_var("AGOS_RATE_LIMIT_COOLDOWN_SECS", "5");
        assert_eq!(rate_limit_cooldown_secs(), 5);
        std::env::set_var("AGOS_RATE_LIMIT_COOLDOWN_SECS", "nonsense");
        assert_eq!(rate_limit_cooldown_secs(), 60, "bad input falls back");
        match previous {
            Some(v) => std::env::set_var("AGOS_RATE_LIMIT_COOLDOWN_SECS", v),
            None => std::env::remove_var("AGOS_RATE_LIMIT_COOLDOWN_SECS"),
        }
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
                    kind: ProviderKind::OpenAI,
                    extra_headers: BTreeMap::new(),
                    masking_server_id: None,
                    shared: false,
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

    #[tokio::test]
    async fn an_image_refusal_clears_vision_instead_of_parking_the_key() {
        // The regression this guards: an image sent to a text-only model used to
        // mark the entry Unhealthy, so *every* later request — text included —
        // was served by nothing until an operator intervened.
        let (store, targets) = setup();
        let entry_id = targets[0].entry.id;
        let route_id = targets[0].entry.route_id;
        let store = Arc::new(store);
        assert!(targets[0].entry.capabilities.vision);

        let result =
            execute_with_failover(store.clone(), targets, Duration::from_secs(5), |_| async {
                Err(anyhow::Error::new(
                    crate::adapter::outbound::ProviderError {
                        status: reqwest::StatusCode::BAD_REQUEST,
                        body: r#"{"error":{"message":"This model does not support image input."}}"#
                            .to_string(),
                        retry_after: None,
                    },
                ))
            })
            .await;
        assert!(result.is_err(), "the refusal still surfaces to the caller");

        let entry = store
            .route_entries(route_id)
            .unwrap()
            .into_iter()
            .find(|e| e.id == entry_id)
            .unwrap();
        assert_eq!(entry.status, ModelStatus::Healthy, "the key is not sick");
        assert!(!entry.capabilities.vision, "vision must be cleared");
        assert!(entry.capabilities.tools, "other capabilities are untouched");
        assert_eq!(entry.cooldown_until, 0, "no cooldown for a capability gap");

        // Text traffic still resolves to the entry...
        let profile_id = pid(&store);
        let text = resolve_targets_with_strategy(
            &store,
            &profile_id,
            "prog/r1",
            RequestNeeds::default(),
            &RoutingState::default(),
        )
        .unwrap();
        assert_eq!(text.len(), 1, "text requests must keep working");

        // ...while image traffic now skips it instead of retrying the refusal.
        let images = resolve_targets_with_strategy(
            &store,
            &profile_id,
            "prog/r1",
            RequestNeeds {
                vision: true,
                ..Default::default()
            },
            &RoutingState::default(),
        )
        .unwrap();
        assert!(images.is_empty(), "image requests skip the entry");
    }

    #[test]
    fn nested_route_entries_expand_to_the_target_routes_leaves() {
        let store = Store::open_in_memory().unwrap();
        let profile = store.create_profile("coder1", None, None).unwrap();
        let p = store
            .create_provider(
                profile.id.as_str(),
                NewProvider {
                    name: "p".into(),
                    description: None,
                    base_url: "https://a.example".into(),
                    auth_token: "tok".into(),
                    kind: ProviderKind::OpenAI,
                    extra_headers: BTreeMap::new(),
                    masking_server_id: None,
                    shared: false,
                },
            )
            .unwrap();
        let proxy = store.create_proxy(profile.id.as_str(), "prog", None).unwrap();
        // A leaf route holding the real model...
        let leaf = store
            .create_route(proxy.id, "leaf", None, RoutingStrategy::Priority, None)
            .unwrap();
        store
            .add_route_entry(leaf.id, p.id, "m1", 1, 1.0, Default::default())
            .unwrap();
        // ...and a parent route that selects the leaf as a model.
        let parent = store
            .create_route(proxy.id, "parent", None, RoutingStrategy::Priority, None)
            .unwrap();
        store
            .add_route_entry_ref(parent.id, leaf.id, "prog/leaf", 1, 1.0)
            .unwrap();

        let targets = resolve_targets(&store, &profile.id, "prog/parent").unwrap();
        assert_eq!(targets.len(), 1, "the leaf's targets are spliced in");
        assert_eq!(targets[0].entry.model_id, "m1");
        assert_eq!(targets[0].provider.name, "p");
    }

    #[test]
    fn a_circular_route_chain_is_refused_rather_than_looped() {
        let store = Store::open_in_memory().unwrap();
        let profile = store.create_profile("coder1", None, None).unwrap();
        let proxy = store.create_proxy(profile.id.as_str(), "prog", None).unwrap();
        let a = store
            .create_route(proxy.id, "a", None, RoutingStrategy::Priority, None)
            .unwrap();
        let b = store
            .create_route(proxy.id, "b", None, RoutingStrategy::Priority, None)
            .unwrap();
        store.add_route_entry_ref(a.id, b.id, "prog/b", 1, 1.0).unwrap();
        store.add_route_entry_ref(b.id, a.id, "prog/a", 1, 1.0).unwrap();

        let err = resolve_targets(&store, &profile.id, "prog/a").unwrap_err();
        assert!(
            err.to_string().contains("circular route reference"),
            "got: {err}"
        );
    }
}

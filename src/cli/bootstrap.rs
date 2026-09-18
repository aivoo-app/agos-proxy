//! Non-interactive profile setup from a JSON description.
//!
//! The interactive wizards are great at a terminal but awkward inside
//! containers and CI. `bootstrap` takes a single JSON document describing a
//! profile, its providers, proxies, routes and route entries, and writes the
//! whole tree to the store in one go — printing the generated API token so an
//! orchestrator can capture it.

use anyhow::{bail, Context as _, Result};
use clap::Subcommand;
use serde::Deserialize;

use crate::cli::util::open_store;
use crate::domain::{ProviderKind, RouteCapabilities, RoutingStrategy};
use crate::storage::NewProvider;

/// Subcommands under `agos-proxy bootstrap`.
#[derive(Debug, Subcommand)]
pub enum BootstrapArgs {
    /// Seed the store from a JSON setup file and print the API token.
    FromFile {
        /// Path to the JSON setup document, or `-` for stdin.
        path: String,
    },
}

/// Entry point for `agos-proxy bootstrap ...`.
pub fn run(args: BootstrapArgs) -> Result<()> {
    let BootstrapArgs::FromFile { path } = args;
    let raw = if path == "-" {
        use std::io::Read as _;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("reading the setup document from stdin")?;
        buf
    } else {
        std::fs::read_to_string(&path).context("reading the setup document")?
    };

    let setup: Setup = serde_json::from_str(&raw).context("parsing the setup document as JSON")?;
    apply(setup)
}

/// Everything `bootstrap` accepts, mirroring the interactive wizards.
#[derive(Debug, Deserialize)]
pub struct Setup {
    /// Profile name; becomes the tenant everything else hangs off.
    pub profile: String,
    /// Optional free-text description for the profile.
    #[serde(default)]
    pub description: Option<String>,
    /// Optional explicit API token (bearer) for the profile. When omitted, a
    /// random one is generated and printed. Useful for scripted or container
    /// provisioning where the caller must know the token ahead of time.
    #[serde(default)]
    pub token: Option<String>,
    /// Upstreams to register under the profile.
    #[serde(default)]
    pub providers: Vec<ProviderSpec>,
    /// Proxies with their routes to expose.
    #[serde(default)]
    pub proxies: Vec<ProxySpec>,
}

/// One upstream provider.
#[derive(Debug, Deserialize)]
pub struct ProviderSpec {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub base_url: String,
    /// Upstream API token; stored encrypted at rest.
    pub auth_token: String,
    /// Defaults to `openai`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Extra headers sent with every upstream request.
    #[serde(default)]
    pub extra_headers: std::collections::BTreeMap<String, String>,
    /// Publish this provider to every profile on the instance.
    #[serde(default)]
    pub shared: Option<bool>,
}

/// One proxy and the routes under it.
#[derive(Debug, Deserialize)]
pub struct ProxySpec {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub routes: Vec<RouteSpec>,
}

/// One addressable fallback chain.
#[derive(Debug, Deserialize)]
pub struct RouteSpec {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Defaults to `priority`.
    #[serde(default)]
    pub strategy: Option<String>,
    /// Optional identity description. When set, the proxy injects a system
    /// message telling the model to adopt this identity instead of revealing
    /// its original model name or developer.
    #[serde(default)]
    pub identity: Option<String>,
    /// Economy max_tokens ceiling (0 = passthrough).
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Economy exact-cache TTL seconds (0 = disabled).
    #[serde(default)]
    pub cache_ttl_secs: Option<i64>,
    /// Models in the chain, in the order they were declared.
    #[serde(default)]
    pub models: Vec<ModelSpec>,
}

/// One model entry inside a route chain.
#[derive(Debug, Deserialize)]
pub struct ModelSpec {
    /// Provider `name` the entry points at.
    pub provider: String,
    /// Upstream model identifier, e.g. `provider/model`.
    pub model: String,
    /// Lower wins; defaults to declaration order.
    #[serde(default)]
    pub priority: Option<i32>,
    /// Weighted-strategy share; defaults to 1.0.
    #[serde(default)]
    pub weight: Option<f64>,
    /// Blended price USD/1M tokens for Economy sorting.
    #[serde(default)]
    pub price_per_1m: Option<f64>,
    /// Capability flags, defaulting to everything off (matching the interactive
    /// wizard) so imports never overclaim tools/vision/JSON support.
    #[serde(default = "default_capabilities")]
    pub capabilities: CapabilitiesSpec,
}

/// Capability flags for a route entry.
#[derive(Debug, Default, Deserialize)]
pub struct CapabilitiesSpec {
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub json_mode: bool,
    /// Maximum context window in tokens; `None` if unknown.
    #[serde(default)]
    pub max_context: Option<u32>,
}

/// `CapabilitiesSpec` default (used when the object itself is omitted): every
/// capability disabled (matching interactive wizard), context window unknown.
fn default_capabilities() -> CapabilitiesSpec {
    CapabilitiesSpec {
        tools: false,
        vision: false,
        json_mode: false,
        max_context: None,
    }
}

fn parse_kind(raw: Option<&str>) -> Result<ProviderKind> {
    match raw.unwrap_or("openai") {
        "openai" => Ok(ProviderKind::OpenAI),
        "openai_responses" => Ok(ProviderKind::OpenAIResponses),
        "anthropic" => Ok(ProviderKind::Anthropic),
        "google" => Ok(ProviderKind::Google),
        // Same alias `provider add` accepts: custom providers are OpenAI
        // passthrough, so a setup document may declare them directly.
        "custom" => Ok(ProviderKind::Custom),
        other => bail!("unknown provider kind {other:?}"),
    }
}

fn parse_strategy(raw: Option<&str>) -> Result<RoutingStrategy> {
    match raw.unwrap_or("priority") {
        "priority" => Ok(RoutingStrategy::Priority),
        "round_robin" => Ok(RoutingStrategy::RoundRobin),
        "weighted" => Ok(RoutingStrategy::Weighted),
        "economy" => Ok(RoutingStrategy::Economy),
        other => bail!("unknown routing strategy {other:?}"),
    }
}

/// Write the setup tree to the store and print a summary + the API token.
///
/// The seed is atomic: if any step fails after the profile row is created, the
/// profile (and everything inserted under it) is removed again so a retry
/// starts from a clean slate.
fn apply(setup: Setup) -> Result<()> {
    let store = open_store()?;

    let profile = match setup.token.as_deref() {
        Some(token) => store.create_profile_with_token(
            &setup.profile,
            setup.description.as_deref(),
            None,
            token,
        )?,
        None => store.create_profile(&setup.profile, setup.description.as_deref(), None)?,
    };

    let seeded = seed(&store, &profile, &setup);
    if let Err(err) = seeded {
        // Compensate: drop the partially-created tree before reporting.
        let _ = store.delete_profile(profile.id.as_str());
        return Err(err.context(format!(
            "bootstrap failed; removed partially created profile {:?}",
            profile.name
        )));
    }

    println!();
    println!(
        "API token (bearer) for {profile_name}:",
        profile_name = profile.name
    );
    println!("{}", profile.id);
    Ok(())
}

/// Insert providers, proxies, routes and route entries for a fresh profile.
fn seed(
    store: &crate::storage::Store,
    profile: &crate::domain::Profile,
    setup: &Setup,
) -> Result<()> {
    println!("profile: {}", profile.name);

    let mut provider_ids = std::collections::BTreeMap::new();
    for spec in &setup.providers {
        let provider = store.create_provider(
            &profile.id,
            NewProvider {
                name: spec.name.clone(),
                description: spec.description.clone(),
                base_url: spec.base_url.trim_end_matches('/').to_string(),
                auth_token: spec.auth_token.clone(),
                kind: parse_kind(spec.kind.as_deref())?,
                extra_headers: spec.extra_headers.clone(),
                masking_server_id: None,
                shared: spec.shared.unwrap_or(false),
            },
        )?;
        provider_ids.insert(spec.name.clone(), provider.id);
        println!("provider: {} ({})", provider.name, provider.base_url);
    }

    for proxy_spec in &setup.proxies {
        let proxy = store.create_proxy(
            &profile.id,
            &proxy_spec.name,
            proxy_spec.description.as_deref(),
        )?;
        println!("proxy: {}", proxy.name);

        for route_spec in &proxy_spec.routes {
            let route = store.create_route(
                proxy.id,
                &route_spec.name,
                route_spec.description.as_deref(),
                parse_strategy(route_spec.strategy.as_deref())?,
                route_spec.identity.as_deref(),
            )?;
            if route_spec.max_tokens.unwrap_or(0) > 0 || route_spec.cache_ttl_secs.unwrap_or(0) > 0
            {
                let _ = store.set_route_economy(
                    route.id,
                    route_spec.max_tokens.unwrap_or(0),
                    route_spec.cache_ttl_secs.unwrap_or(0).max(0),
                );
            }
            println!("route: {}/{}", proxy.name, route.name);

            for (index, model_spec) in route_spec.models.iter().enumerate() {
                let provider_id = *provider_ids.get(&model_spec.provider).ok_or_else(|| {
                    anyhow::anyhow!(
                        "route {}/{} references unknown provider {:?}",
                        proxy.name,
                        route.name,
                        model_spec.provider
                    )
                })?;
                let entry = store.add_route_entry(
                    route.id,
                    provider_id,
                    &model_spec.model,
                    model_spec.priority.unwrap_or(index as i32),
                    model_spec.weight.unwrap_or(1.0),
                    RouteCapabilities {
                        tools: model_spec.capabilities.tools,
                        vision: model_spec.capabilities.vision,
                        json_mode: model_spec.capabilities.json_mode,
                        max_context: model_spec.capabilities.max_context,
                    },
                )?;
                if let Some(p) = model_spec.price_per_1m {
                    let _ = store.set_route_entry_price(entry.id, p);
                }
                println!(
                    "  model: {} (provider {}, priority {})",
                    model_spec.model,
                    model_spec.provider,
                    model_spec.priority.unwrap_or(index as i32)
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kind_accepts_known_tags_and_rejects_unknown() {
        assert!(matches!(parse_kind(None), Ok(ProviderKind::OpenAI)));
        assert!(matches!(
            parse_kind(Some("anthropic")),
            Ok(ProviderKind::Anthropic)
        ));
        assert!(matches!(
            parse_kind(Some("google")),
            Ok(ProviderKind::Google)
        ));
        assert!(matches!(
            parse_kind(Some("openai_responses")),
            Ok(ProviderKind::OpenAIResponses)
        ));
        assert!(matches!(
            parse_kind(Some("custom")),
            Ok(ProviderKind::Custom)
        ));
        assert!(parse_kind(Some("nope")).is_err());
    }

    /// The kind tags embedded in the shipped docker-compose seed must be ones
    /// the CLI actually parses. This guards the docs/config ↔ code contract:
    /// a rename that only lands in the examples (as happened with
    /// `openai_compatible` → `generic`) would otherwise ship a compose stack
    /// whose bootstrap seed is rejected at first run.
    #[test]
    fn compose_seed_only_uses_kinds_the_cli_accepts() {
        let raw = include_str!("../../docker-compose.yml");
        let start = raw
            .find("AGOS_SETUP: |")
            .expect("docker-compose.yml carries an AGOS_SETUP heredoc");
        let mut json = String::new();
        for line in raw[start..].lines().skip(1) {
            match line.strip_prefix("        ") {
                Some(rest) => {
                    json.push_str(rest);
                    json.push('\n');
                }
                None => break,
            }
        }
        let setup: Setup =
            serde_json::from_str(&json).expect("the AGOS_SETUP heredoc must be valid setup JSON");
        assert!(!setup.providers.is_empty(), "compose seed has no providers");
        for provider in &setup.providers {
            assert!(
                parse_kind(provider.kind.as_deref()).is_ok(),
                "docker-compose.yml provider {:?} declares kind {:?}, which the CLI rejects",
                provider.name,
                provider.kind
            );
        }
    }

    #[test]
    fn parse_strategy_accepts_known_tags_and_defaults_to_priority() {
        assert!(matches!(
            parse_strategy(None),
            Ok(RoutingStrategy::Priority)
        ));
        assert!(matches!(
            parse_strategy(Some("round_robin")),
            Ok(RoutingStrategy::RoundRobin)
        ));
        assert!(matches!(
            parse_strategy(Some("weighted")),
            Ok(RoutingStrategy::Weighted)
        ));
        assert!(matches!(
            parse_strategy(Some("economy")),
            Ok(RoutingStrategy::Economy)
        ));
        assert!(parse_strategy(Some("nope")).is_err());
    }

    #[test]
    fn capabilities_default_to_disabled() {
        let setup: Setup = serde_json::from_str(
            r#"{
                "profile": "p",
                "providers": [],
                "proxies": []
            }"#,
        )
        .unwrap();
        assert_eq!(setup.profile, "p");
        // A model spec with no capability flags should come out all-false
        // (matching interactive wizard defaults).
        let model: ModelSpec = serde_json::from_str(r#"{"provider":"x","model":"m"}"#).unwrap();
        assert!(
            !model.capabilities.tools
                && !model.capabilities.vision
                && !model.capabilities.json_mode
        );
    }
}

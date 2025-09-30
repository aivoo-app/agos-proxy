//! Route management: addressable fallback chains of models inside a proxy.

use anyhow::{bail, Result};
use clap::Subcommand;
use dialoguer::{theme::ColorfulTheme, Input, Select};

use anyhow::Context as _;
use crate::cli::util::{open_store, require_profile};
use crate::domain::{ModelStatus, RouteCapabilities, RoutingStrategy};

/// Subcommands under `agos route`.
#[derive(Debug, Subcommand)]
pub enum RouteArgs {
    /// Create a new route under a proxy (interactive wizard, or flag-driven).
    Create {
        /// Name of the owning proxy.
        #[arg(long)]
        proxy: Option<String>,
    },
    /// Show the live health state of every model in a route's chain.
    Status {
        /// Name of the route.
        #[arg(long)]
        route: Option<String>,
    },
}

/// Entry point for `agos route ...`.
pub fn run(args: RouteArgs) -> Result<()> {
    let store = open_store()?;
    match args {
        RouteArgs::Create { proxy } => create(&store, proxy),
        RouteArgs::Status { route } => status(&store, route),
    }
}

fn create(store: &crate::storage::Store, proxy_name: Option<String>) -> Result<()> {
    let theme = ColorfulTheme::default();
    let proxy_name = match proxy_name {
        Some(p) if !p.is_empty() => p,
        _ => Input::<String>::with_theme(&theme)
            .with_prompt("Proxy name")
            .interact_text()?,
    };
    let profile_name: String = Input::<String>::with_theme(&theme)
        .with_prompt("Owning profile name")
        .interact_text()?;
    let profile = require_profile(store, &profile_name)?;
    let proxy = store
        .get_proxy_named(profile.id.as_str(), &proxy_name)?
        .with_context(|| format!("no proxy named {proxy_name:?} under {:?}", profile.name))?;

    let route_name: String = Input::<String>::with_theme(&theme)
        .with_prompt("Route name (e.g. php-developer-3.5-flash)")
        .interact_text()?;
    if store.get_route_named(proxy.id, &route_name)?.is_some() {
        bail!("a route named {route_name:?} already exists under proxy {:?}", proxy.name);
    }
    let description: String = Input::<String>::with_theme(&theme)
        .with_prompt("Description (optional)")
        .allow_empty(true)
        .interact_text()?;

    let route = store.create_route(
        proxy.id,
        &route_name,
        if description.is_empty() {
            None
        } else {
            Some(description.as_str())
        },
        RoutingStrategy::Priority,
    )?;

    println!("Add at least one model to the route's fallback chain.");
    let mut priority = 1i32;
    loop {
        match prompt_route_entry(store, &profile, &theme, route.id, priority)? {
            Some(model_id) => {
                println!("  added model {model_id} at priority {priority}");
                priority += 1;
            }
            None => break,
        }
    }

    println!(
        "Created route {:?} under proxy {:?} / profile {:?} (id {}).",
        route.name, proxy.name, profile.name, route.id
    );
    Ok(())
}

fn prompt_route_entry(
    store: &crate::storage::Store,
    profile: &crate::domain::Profile,
    theme: &ColorfulTheme,
    route_id: i64,
    priority: i32,
) -> Result<Option<String>> {
    let model_id: String = Input::<String>::with_theme(theme)
        .with_prompt("Model ID (e.g. deepseek-v4-flash, empty to finish)")
        .allow_empty(true)
        .interact_text()?;
    if model_id.is_empty() {
        return Ok(None);
    }
    let providers = store.list_providers(profile.id.as_str())?;
    if providers.is_empty() {
        bail!(
            "no providers configured for {:?}; add one with `agos provider add` first",
            profile.name
        );
    }
    let labels: Vec<String> = providers
        .iter()
        .map(|p| format!("{} ({})", p.name, p.base_url))
        .collect();
    let idx = Select::with_theme(theme)
        .with_prompt("Provider")
        .items(&labels)
        .interact()?;
    let provider = &providers[idx];
    let weight_str: String = Input::<String>::with_theme(theme)
        .with_prompt("Weight")
        .default("1.0".into())
        .interact_text()?;
    let weight: f64 = weight_str.parse().unwrap_or(1.0);
    store.add_route_entry(
        route_id,
        provider.id,
        &model_id,
        priority,
        weight,
        RouteCapabilities::default(),
    )?;
    Ok(Some(model_id))
}

fn status(store: &crate::storage::Store, route_name: Option<String>) -> Result<()> {
    let theme = ColorfulTheme::default();
    let route_name = match route_name {
        Some(r) if !r.is_empty() => r,
        _ => Input::<String>::with_theme(&theme)
            .with_prompt("Route name")
            .interact_text()?,
    };
    let profile_name: String = Input::<String>::with_theme(&theme)
        .with_prompt("Owning profile name")
        .interact_text()?;
    let profile = require_profile(store, &profile_name)?;
    let proxy_name: String = Input::<String>::with_theme(&theme)
        .with_prompt("Owning proxy name")
        .interact_text()?;
    let proxy = store
        .get_proxy_named(profile.id.as_str(), &proxy_name)?
        .with_context(|| format!("no proxy named {proxy_name:?} under {:?}", profile.name))?;
    let route = store
        .get_route_named(proxy.id, &route_name)?
        .with_context(|| format!("no route named {route_name:?} under proxy {:?}", proxy.name))?;
    let entries = store.route_entries(route.id)?;
    if entries.is_empty() {
        println!("Route {:?} has no model entries yet.", route.name);
        return Ok(());
    }
    println!(
        "Route {:?} (proxy {:?}, strategy: {:?})",
        route.name, proxy.name, route.strategy
    );
    println!(
        "{:<5} {:<6} {:<8} {:<24} {:<24} {}",
        "PRI", "ID", "STATUS", "MODEL", "PROVIDER", "WEIGHT"
    );
    for e in entries {
        let provider = store.get_provider(e.provider_id)?;
        let provider_name = provider.map(|p| p.name).unwrap_or("(unknown)".to_string());
        println!(
            "{:<5} {:<6} {:<8} {:<24} {:<24} {}",
            e.priority,
            e.id,
            status_label(&e.status),
            e.model_id,
            provider_name,
            e.weight
        );
    }
    Ok(())
}

fn status_label(status: &ModelStatus) -> &'static str {
    match status {
        ModelStatus::Healthy => "healthy",
        ModelStatus::Degraded => "degraded",
        ModelStatus::Unhealthy => "unhealthy",
        ModelStatus::Disabled => "disabled",
    }
}

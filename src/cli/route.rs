//! Route management: addressable fallback chains of models inside a proxy.

use anyhow::{bail, Result};
use clap::Subcommand;
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};

use crate::cli::util::{
    ensure_password_ok, open_store, pick_profile, pick_provider, pick_proxy, pick_route,
};
use crate::domain::{ModelStatus, RouteCapabilities, RoutingStrategy};
use anyhow::Context as _;

/// Subcommands under `agos-proxy route`.
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
    /// Edit a route's name, description and routing strategy.
    Edit {
        /// Name of the owning proxy.
        #[arg(long)]
        proxy: Option<String>,
    },
    /// Remove a route and its model chain.
    Delete {
        /// Name of the owning proxy.
        #[arg(long)]
        proxy: Option<String>,
    },
    /// Manage the models in a route's fallback chain.
    #[command(subcommand)]
    Model(ModelArgs),
}

/// Subcommands under `agos-proxy route model`.
#[derive(Debug, Subcommand)]
pub enum ModelArgs {
    /// Add a model to a route's chain.
    Add {
        /// Name of the owning proxy.
        #[arg(long)]
        proxy: Option<String>,
    },
    /// Remove a model from a route's chain.
    Remove {
        /// Name of the owning proxy.
        #[arg(long)]
        proxy: Option<String>,
    },
    /// Move a model to a new position in the chain.
    Move {
        /// Name of the owning proxy.
        #[arg(long)]
        proxy: Option<String>,
    },
}

/// Entry point for `agos-proxy route ...`.
pub fn run(args: RouteArgs) -> Result<()> {
    let store = open_store()?;
    match args {
        RouteArgs::Create { proxy } => create(&store, proxy),
        RouteArgs::Status { route } => status(&store, route),
        RouteArgs::Edit { proxy } => edit(&store, proxy),
        RouteArgs::Delete { proxy } => delete(&store, proxy),
        RouteArgs::Model(ModelArgs::Add { proxy }) => model_add(&store, proxy),
        RouteArgs::Model(ModelArgs::Remove { proxy }) => model_remove(&store, proxy),
        RouteArgs::Model(ModelArgs::Move { proxy }) => model_move(&store, proxy),
    }
}

/// Resolve the owning profile then pick a proxy, using a flag when provided.
fn resolve_proxy(
    store: &crate::storage::Store,
    given: Option<String>,
) -> Result<(crate::domain::Profile, crate::domain::Proxy)> {
    let profile = pick_profile(store, "Profile")?;
    let mut found: Option<crate::domain::Proxy> = None;
    if let Some(name) = given {
        if !name.is_empty() {
            let p = store
                .get_proxy_named(profile.id.as_str(), &name)?
                .with_context(|| format!("no proxy named {name:?} under {:?}", profile.name))?;
            found = Some(p);
        }
    }
    if !found.is_some() {
        found = Some(pick_proxy(store, &profile, "Proxy")?);
    }
    Ok((profile, found.unwrap()))
}

fn create(store: &crate::storage::Store, proxy_name: Option<String>) -> Result<()> {
    let theme = ColorfulTheme::default();
    let (profile, proxy) = resolve_proxy(store, proxy_name)?;
    ensure_password_ok(&profile)?;

    let route_name: String = Input::<String>::with_theme(&theme)
        .with_prompt("Route name (e.g. php-developer-3.5-flash)")
        .interact_text()?;
    if store.get_route_named(proxy.id, &route_name)?.is_some() {
        bail!(
            "a route named {route_name:?} already exists under proxy {:?}",
            proxy.name
        );
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
    while let Some(model_id) = prompt_route_entry(store, &profile, &theme, route.id, priority)? {
        println!("  added model {model_id} at priority {priority}");
        priority += 1;
    }

    println!(
        "Created route {:?} under proxy {:?} / profile {:?} (id {}).",
        route.name, proxy.name, profile.name, route.id
    );
    Ok(())
}

fn edit(store: &crate::storage::Store, proxy_name: Option<String>) -> Result<()> {
    let theme = ColorfulTheme::default();
    let (profile, proxy) = resolve_proxy(store, proxy_name)?;
    ensure_password_ok(&profile)?;
    let route = pick_route(store, &proxy, "Route to edit")?;

    let route_name: String = Input::<String>::with_theme(&theme)
        .with_prompt("Route name")
        .default(route.name.clone())
        .interact_text()?;
    if route_name != route.name && store.get_route_named(proxy.id, &route_name)?.is_some() {
        bail!(
            "a route named {route_name:?} already exists under proxy {:?}",
            proxy.name
        );
    }
    let description: String = Input::<String>::with_theme(&theme)
        .with_prompt("Description (optional)")
        .default(
            route
                .description
                .as_deref()
                .map(|d| d.to_string())
                .unwrap_or_default(),
        )
        .allow_empty(true)
        .interact_text()?;
    let strategy = pick_strategy(&theme)?;

    store.update_route(
        route.id,
        &route_name,
        if description.is_empty() {
            None
        } else {
            Some(description.as_str())
        },
        strategy,
    )?;
    println!(
        "Updated route {:?} under proxy {:?}.",
        route_name, proxy.name
    );
    Ok(())
}

fn delete(store: &crate::storage::Store, proxy_name: Option<String>) -> Result<()> {
    let theme = ColorfulTheme::default();
    let (profile, proxy) = resolve_proxy(store, proxy_name)?;
    ensure_password_ok(&profile)?;
    let route = pick_route(store, &proxy, "Route to delete")?;
    let sure = Confirm::with_theme(&theme)
        .with_prompt(format!(
            "Delete route {:?} and its model chain?",
            route.name
        ))
        .default(false)
        .interact()?;
    if !sure {
        println!("Cancelled.");
        return Ok(());
    }
    store.delete_route(route.id)?;
    println!("Deleted route {:?}.", route.name);
    Ok(())
}

/// Add a model to an existing route.
fn model_add(store: &crate::storage::Store, proxy_name: Option<String>) -> Result<()> {
    let theme = ColorfulTheme::default();
    let (profile, proxy) = resolve_proxy(store, proxy_name)?;
    ensure_password_ok(&profile)?;
    let route = pick_route(store, &proxy, "Route")?;
    let existing = store.route_entries(route.id)?;
    let next_priority = existing.len() as i32 + 1;
    match prompt_route_entry(store, &profile, &theme, route.id, next_priority)? {
        Some(model_id) => {
            println!(
                "Added model {model_id} to route {:?} at priority {next_priority}.",
                route.name
            );
        }
        None => println!("No model entered; nothing added."),
    }
    Ok(())
}

fn model_remove(store: &crate::storage::Store, proxy_name: Option<String>) -> Result<()> {
    let theme = ColorfulTheme::default();
    let (profile, proxy) = resolve_proxy(store, proxy_name)?;
    ensure_password_ok(&profile)?;
    let route = pick_route(store, &proxy, "Route")?;
    let entry = pick_entry(store, &route, "Model to remove")?;
    let sure = Confirm::with_theme(&theme)
        .with_prompt(format!("Remove model {:?} from the chain?", entry.model_id))
        .default(false)
        .interact()?;
    if !sure {
        println!("Cancelled.");
        return Ok(());
    }
    store.delete_route_entry(entry.id)?;
    println!("Removed model {:?}.", entry.model_id);
    Ok(())
}

fn model_move(store: &crate::storage::Store, proxy_name: Option<String>) -> Result<()> {
    let theme = ColorfulTheme::default();
    let (profile, proxy) = resolve_proxy(store, proxy_name)?;
    ensure_password_ok(&profile)?;
    let route = pick_route(store, &proxy, "Route")?;
    let entries = store.route_entries(route.id)?;
    if entries.is_empty() {
        println!("Route {:?} has no models to reorder.", route.name);
        return Ok(());
    }
    let entry = pick_entry(store, &route, "Model to move")?;
    let new_pos: String = Input::<String>::with_theme(&theme)
        .with_prompt("New priority position (1-based)")
        .interact_text()?;
    let target: i32 = new_pos.parse().unwrap_or(-1);
    let max: i32 = entries.len() as i32;
    if target < 1 || target > max {
        bail!("position must be between 1 and {max}");
    }
    // Shift the chain so priorities stay contiguous, then set the target's slot.
    for (i, e) in entries.iter().enumerate() {
        let p = (i as i32) + 1;
        if e.id != entry.id && p >= target {
            store.set_route_entry_priority(e.id, p + 1)?;
        }
    }
    store.set_route_entry_priority(entry.id, target)?;
    println!("Moved model {:?} to position {}.", entry.model_id, target);
    println!("Run `agos-proxy route status` in the same proxy to see the new order.");
    Ok(())
}

fn pick_entry(
    store: &crate::storage::Store,
    route: &crate::domain::Route,
    prompt: &str,
) -> Result<crate::domain::RouteEntry> {
    let theme = ColorfulTheme::default();
    let entries = store.route_entries(route.id)?;
    if entries.is_empty() {
        bail!("route {:?} has no model entries yet", route.name);
    }
    if entries.len() == 1 {
        return Ok(entries[0].clone());
    }
    let mut labels: Vec<String> = vec![];
    for e in &entries {
        let pname = provider_name(store, e.provider_id)?;
        labels.push(format!(
            "pos {}: {}  (provider {})",
            e.priority, e.model_id, pname
        ));
    }
    let idx = Select::with_theme(&theme)
        .with_prompt(prompt)
        .items(&labels)
        .interact()?;
    Ok(entries[idx].clone())
}

fn provider_name(store: &crate::storage::Store, id: i64) -> Result<String> {
    match store.get_provider(id)? {
        Some(p) => Ok(p.name.clone()),
        None => Ok("(unknown)".to_string()),
    }
}

fn prompt_route_entry(
    store: &crate::storage::Store,
    profile: &crate::domain::Profile,
    theme: &ColorfulTheme,
    route_id: i64,
    priority: i32,
) -> Result<Option<String>> {
    let another = Confirm::with_theme(theme)
        .with_prompt(format!("Add a model at priority {priority}?"))
        .default(true)
        .interact()?;
    if !another {
        return Ok(None);
    }
    let providers = store.list_providers(profile.id.as_str())?;
    if providers.is_empty() {
        bail!(
            "no providers configured for {:?}; add one with `agos-proxy provider add` first",
            profile.name
        );
    }
    let provider = pick_provider(store, profile, "Provider")?;
    let model_id = crate::cli::util::prompt_model_id(
        theme,
        &provider,
        format!("Pick a model from {:?} or enter it manually", provider.name).as_str(),
    )?;
    let weight_str: String = Input::<String>::with_theme(theme)
        .with_prompt("Weight")
        .default("1.0".into())
        .interact_text()?;
    let weight: f64 = weight_str.parse().unwrap_or(1.0);
    let capabilities = prompt_capabilities(theme)?;

    store.add_route_entry(
        route_id,
        provider.id,
        &model_id,
        priority,
        weight,
        capabilities,
    )?;
    Ok(Some(model_id))
}

/// Ask which capabilities the model actually supports so request routing can
/// skip entries that cannot serve tool/vision/json calls.
fn prompt_capabilities(theme: &ColorfulTheme) -> Result<RouteCapabilities> {
    let tools = Confirm::with_theme(theme)
        .with_prompt("Supports tool / function calling?")
        .default(false)
        .interact()?;
    let vision = Confirm::with_theme(theme)
        .with_prompt("Supports image (vision) input?")
        .default(false)
        .interact()?;
    let json_mode = Confirm::with_theme(theme)
        .with_prompt("Supports structured JSON output?")
        .default(false)
        .interact()?;
    Ok(RouteCapabilities {
        tools,
        vision,
        json_mode,
        max_context: None,
    })
}

fn pick_strategy(theme: &ColorfulTheme) -> Result<RoutingStrategy> {
    let labels: Vec<&str> = vec![
        "Priority (strict fallback order)",
        "Round robin (spread across healthy)",
        "Weighted (round robin biased by weight)",
    ];
    let idx = Select::with_theme(theme)
        .with_prompt("Routing strategy")
        .items(&labels)
        .default(0)
        .interact()?;
    Ok(match idx {
        0 => RoutingStrategy::Priority,
        1 => RoutingStrategy::RoundRobin,
        _ => RoutingStrategy::Weighted,
    })
}

fn status(store: &crate::storage::Store, route_name: Option<String>) -> Result<()> {
    let (_profile, proxy) = resolve_proxy(store, None)?;
    let mut route_opt: Option<crate::domain::Route> = None;
    if let Some(r) = route_name {
        if !r.is_empty() {
            let rr = store
                .get_route_named(proxy.id, &r)?
                .with_context(|| format!("no route named {r:?} under proxy {:?}", proxy.name))?;
            route_opt = Some(rr);
        }
    }
    if !route_opt.is_some() {
        route_opt = Some(pick_route(store, &proxy, "Route")?);
    }
    let route: crate::domain::Route = route_opt.unwrap();
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
        "{:<5} {:<6} {:<8} {:<24} {:<24} WEIGHT",
        "PRI", "ID", "STATUS", "MODEL", "PROVIDER"
    );
    for e in entries {
        let provider_name = provider_name(store, e.provider_id)?;
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

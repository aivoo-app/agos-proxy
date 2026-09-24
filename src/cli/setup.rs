//! Guided setup wizard.
//!
//! `agos-proxy setup` walks a first-time user through everything they need to
//! get a working proxy: create (or pick) a profile, then use a small menu to
//! add providers, create proxies, and build route chains — all in one TUI
//! session instead of four separate commands.
//!
//! This command never destroys anything and never asks for automation: it is
//! purely interactive, so it prints the generated API token at the end the
//! same way the individual wizards do. The individual commands remain available
//! for fine-grained, scriptable use.

use anyhow::{bail, Context as _, Result};
use clap::Parser;
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};

use crate::cli::util::{ensure_password_ok, hash_password, kind_label, open_store, prompt_headers};
use crate::domain::{ModelStatus, Profile, ProviderKind, Proxy, RouteCapabilities};
use crate::storage::{NewProvider, Store};

/// Arguments for `agos setup`. None: the command is fully wizard-driven.
#[derive(Debug, Parser)]
pub struct SetupArgs;

/// Entry point for `agos-proxy setup`.
pub fn run(_args: SetupArgs) -> Result<()> {
    let store = open_store()?;
    let theme = ColorfulTheme::default();

    println!();
    println!("AGOS Proxy setup");
    println!("──────────────────────────────────────────────────────────");
    println!("We'll walk through the pieces in order. Each screen is skippable,");
    println!("and read-only `list` / `show` / `status` commands all remain");
    println!("available for fine-grained work after this wizard.");
    println!();

    let profile = choose_profile(&store, &theme)?;
    if let Some(p) = profile {
        println!("Continuing with profile {:?}.", p.name);
        menu_loop(&store, &theme, &p)?;
    }
    Ok(())
}

/// Pick a profile to operate on, creating one if none exist.
fn choose_profile(store: &Store, theme: &ColorfulTheme) -> Result<Option<Profile>> {
    let existing = store.list_profiles()?;
    if existing.is_empty() {
        println!("No profiles yet — let's create the first one.");
        return create_profile(store, theme);
    }

    let mut labels: Vec<String> = vec!["＋  Create a new profile".into()];
    for p in &existing {
        labels.push(format!("{}  ({})", p.name, token_preview(&p.id)));
    }
    let idx = Select::with_theme(theme)
        .with_prompt("Choose a profile (or create a new one)")
        .items(&labels)
        .default(0)
        .interact()?;

    if idx == 0 {
        create_profile(store, theme)
    } else {
        Ok(Some(existing[idx - 1].clone()))
    }
}

/// Create a brand-new profile, printing the API token at the end.
fn create_profile(store: &Store, theme: &ColorfulTheme) -> Result<Option<Profile>> {
    let name: String = Input::<String>::with_theme(theme)
        .with_prompt("Profile name (e.g. coder1)")
        .interact_text()?;
    if store.get_profile_by_name(&name)?.is_some() {
        bail!("a profile named {name:?} already exists");
    }
    let description: Option<String> = Input::<String>::with_theme(theme)
        .with_prompt("Description (optional)")
        .allow_empty(true)
        .interact_text()
        .map(|s| if s.is_empty() { None } else { Some(s) })?;

    let wants_password = Confirm::with_theme(theme)
        .with_prompt("Protect this profile with a password? (used to gate changes)")
        .default(false)
        .interact()?;
    let password_hash: Option<String> = if wants_password {
        let pw = dialoguer::Password::with_theme(theme)
            .with_prompt("Password")
            .interact()?;
        Some(hash_password(&pw)?)
    } else {
        None
    };

    let wants_limit = Confirm::with_theme(theme)
        .with_prompt("Limit API requests per minute? (rate limiting)")
        .default(false)
        .interact()?;
    let rpm_limit = if wants_limit {
        Input::<i64>::with_theme(theme)
            .with_prompt("Requests per minute")
            .default(60)
            .interact()?
    } else {
        0
    };

    let profile = store.create_profile(&name, description.as_deref(), password_hash.as_deref())?;
    if rpm_limit > 0 {
        store.set_profile_rpm_limit(&profile.id, rpm_limit)?;
    }
    println!();
    println!("Created profile {:?}.", profile.name);
    println!("API token (bearer) for {:?}:", profile.name);
    println!("  {}", profile.id);
    println!("Store: {}", store.path().display());
    println!();
    Ok(Some(profile))
}

/// The main action menu, looped until the user chooses to finish.
fn menu_loop(store: &Store, theme: &ColorfulTheme, profile: &Profile) -> Result<()> {
    loop {
        let labels: Vec<&str> = vec![
            "Add a provider (upstream + credentials)",
            "Create a proxy (group of routes)",
            "Create a route (model fallback chain)",
            "Show current setup",
            "───  Finish  ───",
        ];
        let idx = Select::with_theme(theme)
            .with_prompt(format!(
                "What would you like to do?  (profile: {:?})",
                profile.name
            ))
            .items(&labels)
            .interact()?;

        match idx {
            0 => add_provider(store, theme, profile)?,
            1 => create_proxy(store, theme, profile)?,
            2 => create_route(store, theme, profile)?,
            3 => show_summary(store, profile)?,
            _ => {
                println!();
                println!("Setup complete. The proxy/server commands are available any time;");
                println!("use `agos-proxy serve` to start listening and `agos-proxy chat` to");
                println!("test a route interactively.");
                return Ok(());
            }
        }
        println!();
    }
}

/// Add one provider (kind, base URL, credentials, optional headers).
fn add_provider(store: &Store, theme: &ColorfulTheme, profile: &Profile) -> Result<()> {
    ensure_password_ok(profile)?;

    let name: String = Input::<String>::with_theme(theme)
        .with_prompt("Provider name (e.g. upstream)")
        .interact_text()?;
    let base_url: String = Input::<String>::with_theme(theme)
        .with_prompt("Base URL")
        .default("https://api.example.com".into())
        .interact_text()?;
    let auth_token = crate::cli::util::prompt_token(theme, "API token (stored encrypted)", false)?;
    let kind = pick_kind(theme)?;
    let description: String = Input::<String>::with_theme(theme)
        .with_prompt("Description (optional)")
        .allow_empty(true)
        .interact_text()?;
    let extra_headers = prompt_headers()?;

    let provider = store.create_provider(
        profile.id.as_str(),
        NewProvider {
            name,
            description: if description.is_empty() {
                None
            } else {
                Some(description)
            },
            base_url,
            auth_token,
            kind,
            extra_headers,
            masking_server_id: None,
            shared: false,
        },
    )?;
    println!(
        "Added provider {:?} ({}) under profile {:?}.",
        provider.name,
        kind_label(&provider.kind),
        profile.name
    );
    Ok(())
}

/// Create a proxy, then offer to immediately build a route under it.
fn create_proxy(store: &Store, theme: &ColorfulTheme, profile: &Profile) -> Result<()> {
    ensure_password_ok(profile)?;

    let name: String = Input::<String>::with_theme(theme)
        .with_prompt("Proxy name (e.g. Programmer)")
        .interact_text()?;
    if store.get_proxy_named(profile.id.as_str(), &name)?.is_some() {
        bail!(
            "a proxy named {name:?} already exists under {:?}",
            profile.name
        );
    }
    let description: String = Input::<String>::with_theme(theme)
        .with_prompt("Description (optional)")
        .allow_empty(true)
        .interact_text()?;
    let proxy = store.create_proxy(
        profile.id.as_str(),
        &name,
        if description.is_empty() {
            None
        } else {
            Some(description.as_str())
        },
    )?;
    println!(
        "Created proxy {:?} under profile {:?}.",
        proxy.name, profile.name
    );

    let build_route = Confirm::with_theme(theme)
        .with_prompt("Add a route with a fallback chain under this proxy now?")
        .default(true)
        .interact()?;
    if build_route {
        prompt_route(store, theme, profile, &proxy)?;
    }
    Ok(())
}

/// Create a route under a chosen proxy, looping over model entries.
fn create_route(store: &Store, theme: &ColorfulTheme, profile: &Profile) -> Result<()> {
    ensure_password_ok(profile)?;
    let proxy = pick_proxy(store, theme, profile)?;
    prompt_route(store, theme, profile, &proxy)?;
    Ok(())
}

/// The shared "build a route + its entries" flow.
fn prompt_route(
    store: &Store,
    theme: &ColorfulTheme,
    profile: &Profile,
    proxy: &Proxy,
) -> Result<()> {
    let route_name: String = Input::<String>::with_theme(theme)
        .with_prompt("Route name (e.g. php-developer-3.5-flash)")
        .interact_text()?;
    if store.get_route_named(proxy.id, &route_name)?.is_some() {
        bail!(
            "a route named {route_name:?} already exists under proxy {:?}",
            proxy.name
        );
    }
    let description: String = Input::<String>::with_theme(theme)
        .with_prompt("Description (optional)")
        .allow_empty(true)
        .interact_text()?;
    let identity_prompt = Input::<String>::with_theme(theme)
        .with_prompt("Identity description (optional — hides the real model name from users)")
        .allow_empty(true)
        .interact_text()?;
    let identity = if identity_prompt.is_empty() {
        None
    } else {
        Some(identity_prompt)
    };
    let route = store.create_route(
        proxy.id,
        &route_name,
        if description.is_empty() {
            None
        } else {
            Some(description.as_str())
        },
        crate::domain::RoutingStrategy::Priority,
        identity.as_deref(),
    )?;

    println!("Add at least one model to the route's fallback chain.");
    let mut priority = 1i32;
    while let Some(model_id) = prompt_route_entry(store, theme, profile, route.id, priority)? {
        println!("  added model {model_id} at priority {priority}");
        priority += 1;
    }
    println!(
        "Created route {:?} under proxy {:?} / profile {:?}.",
        route.name, proxy.name, profile.name
    );
    Ok(())
}

/// Ask for one model entry: model id, provider, weight, capabilities.
fn prompt_route_entry(
    store: &Store,
    theme: &ColorfulTheme,
    profile: &Profile,
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
            "no providers configured for {:?}; add one with the menu first",
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
    let model_id = crate::cli::util::prompt_model_id(
        theme,
        provider,
        format!("Pick a model from {:?} or enter it manually", provider.name).as_str(),
    )?;
    let weight_str: String = Input::<String>::with_theme(theme)
        .with_prompt("Weight (relative, for weighted routing)")
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

/// Ask which capabilities the model truly supports.
fn prompt_capabilities(theme: &ColorfulTheme) -> Result<RouteCapabilities> {
    let tools = Confirm::with_theme(theme)
        .with_prompt("Supports tool / function calling?")
        .default(false)
        .interact()?;
    let vision = Confirm::with_theme(theme)
        .with_prompt("Supports image (vision) input?")
        .default(false)
        .interact()?;
    let audio = Confirm::with_theme(theme)
        .with_prompt("Supports audio input?")
        .default(false)
        .interact()?;
    let video = Confirm::with_theme(theme)
        .with_prompt("Supports video input?")
        .default(false)
        .interact()?;
    let files = Confirm::with_theme(theme)
        .with_prompt("Supports file/PDF input?")
        .default(false)
        .interact()?;
    let json_mode = Confirm::with_theme(theme)
        .with_prompt("Supports structured JSON output?")
        .default(false)
        .interact()?;
    Ok(RouteCapabilities {
        tools,
        vision,
        audio,
        video,
        files,
        json_mode,
        max_context: None,
    })
}

/// Let the user pick which provider kind (OpenAI, Anthropic, ...).
fn pick_kind(theme: &ColorfulTheme) -> Result<ProviderKind> {
    let kinds = [
        ProviderKind::OpenAI,
        ProviderKind::Anthropic,
        ProviderKind::Google,
        ProviderKind::Custom,
    ];
    let labels: Vec<&str> = kinds.iter().map(|k| kind_label(k)).collect();
    let idx = Select::with_theme(theme)
        .with_prompt("Provider kind")
        .items(&labels)
        .default(0)
        .interact()?;
    Ok(kinds[idx])
}

/// Choose a proxy that honors the route being built; errors if none exist.
fn pick_proxy(store: &Store, theme: &ColorfulTheme, profile: &Profile) -> Result<Proxy> {
    let proxies = store.list_proxies(profile.id.as_str())?;
    if proxies.is_empty() {
        bail!(
            "no proxies configured for {:?}; create one with the menu first",
            profile.name
        );
    }
    let labels: Vec<String> = proxies.iter().map(|p| p.name.clone()).collect();
    let idx = Select::with_theme(theme)
        .with_prompt("Proxy")
        .items(&labels)
        .interact()?;
    let p = &proxies[idx];
    let proxy = store
        .get_proxy_named(profile.id.as_str(), &p.name)?
        .with_context(|| format!("no proxy named {:?} under {:?}", p.name, profile.name))?;
    Ok(proxy)
}

/// Print a compact summary of everything configured under the profile.
fn show_summary(store: &Store, profile: &Profile) -> Result<()> {
    let limit = if profile.rpm_limit > 0 {
        format!("{} rpm", profile.rpm_limit)
    } else {
        "unlimited".to_string()
    };
    println!(
        "Profile {:?} (token: …{}) — rate limit: {limit}",
        profile.name,
        token_preview(&profile.id)
    );

    let providers = store.list_providers(profile.id.as_str())?;
    println!();
    println!("  Providers ({}):", providers.len());
    for p in providers {
        println!(
            "    - {}  [{}]  {}  ({})",
            p.name,
            kind_label(&p.kind),
            p.base_url,
            p.id
        );
    }

    print_proxies(store, profile)?;
    Ok(())
}

/// Proxy -> route -> entries tree, so a user can eyeball their setup.
fn print_proxies(store: &Store, profile: &Profile) -> Result<()> {
    let proxies = store.list_proxies(profile.id.as_str())?;
    println!();
    println!("  Proxies ({}):", proxies.len());
    if proxies.is_empty() {
        println!("    (none)");
        return Ok(());
    }
    for proxy in proxies {
        println!("    - {}  ({})", proxy.name, proxy.id);
        let routes = store.list_routes(proxy.id)?;
        for route in routes {
            println!(
                "        route {}  [strategy: {:?}]",
                route.name, route.strategy
            );
            let entries = store.route_entries(route.id)?;
            for e in entries {
                let status = match e.status {
                    ModelStatus::Healthy => "healthy".to_string(),
                    ModelStatus::Degraded => "degraded".to_string(),
                    ModelStatus::Unhealthy => "unhealthy".to_string(),
                    ModelStatus::Disabled => "disabled".to_string(),
                };
                let provider = e
                    .provider_id
                    .and_then(|id| store.get_provider(id).ok().flatten());
                let provider_name = match provider {
                    Some(pr) => pr.name.clone(),
                    None => {
                        if let Some(tid) = e.target_route_id {
                            store
                                .get_route_by_id(tid)?
                                .map(|r| format!("→ {}", r.name))
                                .unwrap_or_else(|| "(unknown)".into())
                        } else {
                            "(unknown)".into()
                        }
                    }
                };
                println!(
                    "          - {}  (via {}, priority {}, {status})",
                    e.model_id, provider_name, e.priority
                );
            }
        }
    }
    Ok(())
}

/// Short token preview for display (never prints the full secret).
fn token_preview(token: &str) -> String {
    if token.len() > 8 {
        token[token.len() - 8..].to_string()
    } else {
        token.to_string()
    }
}

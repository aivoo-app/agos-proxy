//! Manage egress masks: HTTP hops AGOS relays upstream traffic through.
//!
//! One mask per provider is what gives several keys of the same upstream
//! distinct network identities. This module is the `agos-proxy mask ...`
//! surface: add/list/set/delete the hop records, bind a profile-wide default,
//! and verify what actually egresses (`test`, `audit`).
//!
//! Secrets are never printed: `mask list` shows everything except the shared
//! secret, and the export/import path round-trips it encrypted or not at all.

use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use clap::Subcommand;
use dialoguer::{theme::ColorfulTheme, Input, Select};

use crate::cli::util::{open_store, pick_profile, require_profile};
use crate::domain::MaskingServer;
use crate::mask::EgressIdentity;
use crate::storage::NewMaskingServer;

/// Subcommands under `agos-proxy mask`.
#[derive(Debug, Subcommand)]
pub enum MaskArgs {
    /// Register a new egress hop (interactive wizard, or flag-driven).
    Add {
        /// Name of the owning profile.
        #[arg(long)]
        profile: Option<String>,
        /// Mask name (e.g. `opencode-key-1`); triggers non-interactive mode
        /// when combined with `--endpoint-url` and `--secret`.
        #[arg(long)]
        name: Option<String>,
        /// Backend label: cf_worker | lambda | cloud_run | nginx | vps | commercial.
        #[arg(long)]
        kind: Option<String>,
        /// Absolute URL of the hop.
        #[arg(long)]
        endpoint_url: Option<String>,
        /// Shared secret the hop expects in `X-forward-mask`.
        #[arg(long)]
        secret: Option<String>,
        /// Skip this hop for request bodies larger than this many bytes (0 = no limit).
        #[arg(long, default_value_t = 0)]
        max_body_bytes: i64,
        /// Egress IP the probe must report, when it is stable (e.g. a VPS).
        #[arg(long)]
        expected_egress_ip: Option<String>,
    },
    /// List the masks configured on a profile (secrets are never shown).
    List {
        /// Name of the owning profile.
        #[arg(long)]
        profile: Option<String>,
    },
    /// Change a mask's settings.
    Set {
        /// Name of the owning profile.
        #[arg(long)]
        profile: Option<String>,
        /// Mask to change.
        #[arg(long)]
        mask: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        endpoint_url: Option<String>,
        /// New shared secret (empty to keep the current one).
        #[arg(long)]
        secret: Option<String>,
        #[arg(long)]
        max_body_bytes: Option<i64>,
        /// Expected egress IP; `none` clears the expectation.
        #[arg(long)]
        expected_egress_ip: Option<String>,
    },
    /// Make a mask the profile-wide fallback for providers without their own.
    SetDefault {
        /// Name of the owning profile.
        #[arg(long)]
        profile: Option<String>,
        /// Mask to use as the default, or `none` to clear.
        #[arg(long)]
        mask: Option<String>,
    },
    /// Remove a mask. Providers bound to it fall back to the profile default.
    Delete {
        /// Name of the owning profile.
        #[arg(long)]
        profile: Option<String>,
        /// Name of the mask to remove.
        #[arg(long)]
        mask: Option<String>,
        /// Skip the confirmation prompt (for scripts/CI).
        #[arg(long)]
        yes: bool,
    },
    /// Verify a mask end to end: secret, probe reply and (optionally) that its
    /// egress identity matches expectations. `--repeat` hammers the probe to
    /// measure identity stability (a VPS should be constant; a serverless
    /// platform rotates within its pool).
    Test {
        /// Name of the owning profile.
        #[arg(long)]
        profile: Option<String>,
        /// Mask to test.
        #[arg(long)]
        mask: Option<String>,
        /// Also verify the reported egress IP against `expected_egress_ip`.
        #[arg(long)]
        egress_ip: bool,
        /// How many probes to send.
        #[arg(long, default_value_t = 1)]
        repeat: u32,
    },
    /// Probe every mask on a profile and report the egress identities, warning
    /// when two masks share an ASN (five deployments on one platform are one
    /// upstream-visible identity). Exits non-zero on duplicate ASNs.
    Audit {
        /// Name of the owning profile.
        #[arg(long)]
        profile: Option<String>,
    },
}

/// Entry point for `agos-proxy mask ...`.
pub fn run(args: MaskArgs) -> Result<()> {
    let store = open_store()?;
    match args {
        MaskArgs::Add {
            profile,
            name,
            kind,
            endpoint_url,
            secret,
            max_body_bytes,
            expected_egress_ip,
        } => add(
            &store,
            profile,
            name,
            kind,
            endpoint_url,
            secret,
            max_body_bytes,
            expected_egress_ip,
        ),
        MaskArgs::List { profile } => list(&store, profile),
        MaskArgs::Set {
            profile,
            mask,
            kind,
            endpoint_url,
            secret,
            max_body_bytes,
            expected_egress_ip,
        } => set(
            &store,
            profile,
            mask,
            kind,
            endpoint_url,
            secret,
            max_body_bytes,
            expected_egress_ip,
        ),
        MaskArgs::SetDefault { profile, mask } => set_default(&store, profile, mask),
        MaskArgs::Delete { profile, mask, yes } => delete(&store, profile, mask, yes),
        MaskArgs::Test {
            profile,
            mask,
            egress_ip,
            repeat,
        } => test(&store, profile, mask, egress_ip, repeat),
        MaskArgs::Audit { profile } => audit(&store, profile),
    }
}

fn resolve_profile(
    store: &crate::storage::Store,
    given: Option<String>,
) -> Result<crate::domain::Profile> {
    match given {
        Some(p) if !p.is_empty() => require_profile(store, &p),
        _ => pick_profile(store, "Profile"),
    }
}

fn resolve_mask(
    store: &crate::storage::Store,
    profile: &crate::domain::Profile,
    name: Option<String>,
    prompt: &str,
) -> Result<MaskingServer> {
    let masks = store.list_masking_servers(profile.id.as_str())?;
    match name {
        Some(n) if !n.is_empty() => masks
            .into_iter()
            .find(|m| m.name == n)
            .with_context(|| format!("no mask named {n:?} under profile {:?}", profile.name)),
        _ => {
            if masks.is_empty() {
                bail!(
                    "no masks configured for profile {:?}; add one with `agos-proxy mask add`",
                    profile.name
                );
            }
            let names: Vec<&str> = masks.iter().map(|m| m.name.as_str()).collect();
            let pick = Select::with_theme(&ColorfulTheme::default())
                .with_prompt(prompt)
                .items(&names)
                .default(0)
                .interact()?;
            Ok(masks.into_iter().nth(pick).expect("index from items()"))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn add(
    store: &crate::storage::Store,
    profile: Option<String>,
    name: Option<String>,
    kind: Option<String>,
    endpoint_url: Option<String>,
    secret: Option<String>,
    max_body_bytes: i64,
    expected_egress_ip: Option<String>,
) -> Result<()> {
    let theme = ColorfulTheme::default();
    let profile = resolve_profile(store, profile)?;

    // Flag-driven mode: name + endpoint_url + secret supplied.
    let spec = if let (Some(name), Some(endpoint_url), Some(secret)) =
        (name.clone(), endpoint_url.clone(), secret.clone())
    {
        NewMaskingServer {
            name,
            kind: kind.unwrap_or_else(|| "custom".to_string()),
            endpoint_url: endpoint_url.trim_end_matches('/').to_string(),
            secret,
            max_body_bytes,
            expected_egress_ip,
        }
    } else {
        let name = match name {
            Some(n) => n,
            None => Input::<String>::with_theme(&theme)
                .with_prompt("Mask name")
                .interact_text()?,
        };
        let kinds = [
            "cf_worker",
            "lambda",
            "cloud_run",
            "nginx",
            "vps",
            "commercial",
            "custom",
        ];
        let pick = Select::with_theme(&theme)
            .with_prompt("Backend kind")
            .items(kinds)
            .default(0)
            .interact()?;
        let endpoint_url: String = Input::<String>::with_theme(&theme)
            .with_prompt("Endpoint URL (absolute https://...)")
            .interact_text()?;
        let secret =
            crate::cli::util::prompt_token(&theme, "Shared secret (X-forward-mask)", false)?;
        let max_body: String = Input::<String>::with_theme(&theme)
            .with_prompt("Max body bytes (0 = no limit)")
            .default("0".to_string())
            .allow_empty(true)
            .interact_text()?;
        NewMaskingServer {
            name,
            kind: kinds[pick].to_string(),
            endpoint_url: endpoint_url.trim_end_matches('/').to_string(),
            secret,
            max_body_bytes: max_body.trim().parse().unwrap_or(0),
            expected_egress_ip: None,
        }
    };

    let mask = store.create_masking_server(profile.id.as_str(), spec)?;
    println!(
        "Added mask {:?} ({}) under profile {:?}.",
        mask.name, mask.kind, profile.name
    );
    println!(
        "Verify it with `agos-proxy mask test --profile {} --mask {} --egress-ip`.",
        profile.name, mask.name
    );
    Ok(())
}

fn list(store: &crate::storage::Store, profile: Option<String>) -> Result<()> {
    let profile = resolve_profile(store, profile)?;
    let masks = store.list_masking_servers(profile.id.as_str())?;
    if masks.is_empty() {
        println!(
            "No masks configured for {:?}. Add one with `agos-proxy mask add`.",
            profile.name
        );
        return Ok(());
    }
    let default_id = profile.default_masking_server_id;
    println!(
        "{:<16} {:<12} {:<40} {:>10} {:<16} ENDPOINT",
        "ID", "KIND", "NAME", "MAX BODY", "EGRESS IP"
    );
    for m in masks {
        let default_mark = if Some(m.id) == default_id {
            " (default)"
        } else {
            ""
        };
        println!(
            "{:<16} {:<12} {:<40} {:>10} {:<16} {}",
            m.id,
            m.kind,
            format!("{}{}", m.name, default_mark),
            if m.max_body_bytes > 0 {
                m.max_body_bytes.to_string()
            } else {
                "-".to_string()
            },
            m.last_verified_ip.as_deref().unwrap_or("-"),
            m.endpoint_url
        );
    }
    println!("Secrets are never displayed.");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn set(
    store: &crate::storage::Store,
    profile: Option<String>,
    mask: Option<String>,
    kind: Option<String>,
    endpoint_url: Option<String>,
    secret: Option<String>,
    max_body_bytes: Option<i64>,
    expected_egress_ip: Option<String>,
) -> Result<()> {
    let theme = ColorfulTheme::default();
    let profile = resolve_profile(store, profile)?;
    let current = resolve_mask(store, &profile, mask, "Mask to change")?;

    let endpoint_url = match endpoint_url {
        Some(u) => u.trim_end_matches('/').to_string(),
        None => Input::<String>::with_theme(&theme)
            .with_prompt("Endpoint URL")
            .default(current.endpoint_url.clone())
            .interact_text()?,
    };
    // Empty secret keeps the stored one (prompt_token allows empty when
    // `allow_empty` semantics apply; an explicit `--secret ""` also keeps it).
    let new_secret = match secret {
        Some(s) if !s.is_empty() => s,
        _ => current.secret.clone(),
    };
    let max_body = max_body_bytes.unwrap_or(current.max_body_bytes);
    let expected = match expected_egress_ip.as_deref() {
        Some("none") | Some("") => None,
        Some(ip) => Some(ip.to_string()),
        None => current.expected_egress_ip.clone(),
    };

    store.update_masking_server(
        current.id,
        NewMaskingServer {
            name: current.name.clone(),
            kind: kind.unwrap_or(current.kind.clone()),
            endpoint_url,
            secret: new_secret,
            max_body_bytes: max_body,
            expected_egress_ip: expected,
        },
    )?;
    println!(
        "Updated mask {:?} under profile {:?}.",
        current.name, profile.name
    );
    Ok(())
}

fn set_default(
    store: &crate::storage::Store,
    profile: Option<String>,
    mask: Option<String>,
) -> Result<()> {
    let profile = resolve_profile(store, profile)?;
    let id = match mask.as_deref() {
        Some("none") | Some("") => None,
        _ => {
            let m = resolve_mask(store, &profile, mask, "Default mask")?;
            Some(m.id)
        }
    };
    store.set_profile_default_masking(profile.id.as_str(), id)?;
    match id {
        Some(id) => println!("Profile {:?} default mask set (id {id}).", profile.name),
        None => println!("Profile {:?} default mask cleared.", profile.name),
    }
    Ok(())
}

fn delete(
    store: &crate::storage::Store,
    profile: Option<String>,
    mask: Option<String>,
    yes: bool,
) -> Result<()> {
    let theme = ColorfulTheme::default();
    let profile = resolve_profile(store, profile)?;
    let mask = resolve_mask(store, &profile, mask, "Mask to delete")?;
    if !yes {
        let ok = dialoguer::Confirm::with_theme(&theme)
            .with_prompt(format!("Delete mask {:?}?", mask.name))
            .default(false)
            .interact()?;
        if !ok {
            return Ok(());
        }
    }
    store.delete_masking_server(mask.id)?;
    println!(
        "Deleted mask {:?}. Providers bound to it now fall back to the profile default.",
        mask.name
    );
    Ok(())
}

/// Run an async probe on the CLI thread.
fn tokio_probe(client: &reqwest::Client, mask: &MaskingServer) -> Result<EgressIdentity> {
    tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(crate::mask::probe(client, mask, Duration::from_secs(10)))
}

#[allow(clippy::too_many_arguments)]
fn test(
    store: &crate::storage::Store,
    profile: Option<String>,
    mask: Option<String>,
    check_egress: bool,
    repeat: u32,
) -> Result<()> {
    let profile = resolve_profile(store, profile)?;
    let mask = resolve_mask(store, &profile, mask, "Mask to test")?;
    let client = reqwest::Client::new();

    let mut identities: Vec<EgressIdentity> = Vec::new();
    for i in 0..repeat {
        let id = tokio_probe(&client, &mask)
            .with_context(|| format!("probe {}/{} failed", i + 1, repeat))?;
        println!(
            "  probe {:>2}/{:<2}: ip={} asn={} country={}",
            i + 1,
            repeat,
            id.ip,
            id.asn.as_deref().unwrap_or("-"),
            id.country.as_deref().unwrap_or("-")
        );
        identities.push(id);
    }

    // Remember the freshest identity so `mask list` and `audit` can reason
    // about the profile without re-probing.
    let last = identities.last().expect("at least one probe");
    store.record_mask_probe(
        mask.id,
        &last.ip,
        last.asn.as_deref(),
        last.country.as_deref(),
    )?;

    let distinct: std::collections::BTreeSet<&str> =
        identities.iter().map(|i| i.ip.as_str()).collect();
    println!(
        "{repeat} probe(s), {count} distinct egress IP(s).",
        count = distinct.len()
    );
    if distinct.len() == 1 {
        println!("Stable identity — good for quota bookkeeping.");
    } else {
        println!("Rotating identity — the upstream sees a pool, which only helps if it keys on exact IPs, not ASNs.");
    }

    if check_egress {
        if let Some(expected) = &mask.expected_egress_ip {
            if !identities.iter().any(|i| &i.ip == expected) {
                bail!(
                    "egress check failed: expected ip {expected}, got {}",
                    identities
                        .iter()
                        .map(|i| i.ip.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            println!("Egress IP matches expectation ({expected}).");
        } else {
            println!("No expected_egress_ip set on this mask; skipping the IP check.");
        }
    }
    Ok(())
}

fn audit(store: &crate::storage::Store, profile: Option<String>) -> Result<()> {
    let profile = resolve_profile(store, profile)?;
    let masks = store.list_masking_servers(profile.id.as_str())?;
    if masks.is_empty() {
        println!("No masks configured for {:?}.", profile.name);
        return Ok(());
    }
    let client = reqwest::Client::new();
    println!("Auditing {} mask(s) for {:?}:\n", masks.len(), profile.name);
    println!(
        "{:<20} {:<12} {:<16} {:<20} COUNTRY",
        "MASK", "KIND", "EGRESS IP", "ASN"
    );
    let mut rows: Vec<(MaskingServer, Option<EgressIdentity>)> = Vec::new();
    for m in &masks {
        let id = tokio_probe(&client, m);
        match &id {
            Ok(i) => println!(
                "{:<20} {:<12} {:<16} {:<20} {}",
                m.name,
                m.kind,
                i.ip,
                i.asn.as_deref().unwrap_or("-"),
                i.country.as_deref().unwrap_or("-")
            ),
            Err(e) => println!("{:<20} {:<12} PROBE FAILED: {e:#}", m.name, m.kind),
        }
        if let Ok(i) = &id {
            let _ = store.record_mask_probe(m.id, &i.ip, i.asn.as_deref(), i.country.as_deref());
        }
        rows.push((m.clone(), id.ok()));
    }

    let mut duplicate_asns: Vec<String> = Vec::new();
    let mut by_asn: std::collections::BTreeMap<String, Vec<&str>> = Default::default();
    for (m, id) in &rows {
        if let Some(i) = id {
            if let Some(asn) = &i.asn {
                by_asn.entry(asn.clone()).or_default().push(&m.name);
            }
        }
    }
    for (asn, names) in &by_asn {
        if names.len() > 1 {
            duplicate_asns.push(format!("{asn}: {}", names.join(", ")));
        }
    }
    println!();
    if duplicate_asns.is_empty() {
        println!("OK: every mask reports a distinct ASN — the keys present distinct identities upstream.");
        Ok(())
    } else {
        println!(
            "WARNING: masks sharing an ASN are one identity as far as an upstream is concerned:"
        );
        for d in &duplicate_asns {
            println!("  {d}");
        }
        println!("Spread the masks across different platforms/providers/regions.");
        bail!("duplicate ASNs detected");
    }
}

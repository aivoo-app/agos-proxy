//! Surfacing the usage log: per-request records and per-model aggregates.

use anyhow::Result;
use clap::Subcommand;
use dialoguer::{theme::ColorfulTheme, Input};

use crate::cli::util::{open_store, require_profile};

/// Subcommands under `agos-proxy usage`.
#[derive(Debug, Subcommand)]
pub enum UsageArgs {
    /// Aggregate calls, failures, latency and tokens per model.
    Stats {
        /// Name of the profile to report on.
        #[arg(long)]
        profile: Option<String>,
        /// Aggregate per upstream key (provider) instead of per model, showing
        /// each key's egress mask and how often it was rate limited.
        #[arg(long)]
        by_key: bool,
    },
    /// The most recent individual requests, newest first.
    Recent {
        /// Name of the profile to report on.
        #[arg(long)]
        profile: Option<String>,
        /// How many records to show.
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
}

/// Entry point for `agos-proxy usage ...`.
pub fn run(args: UsageArgs) -> Result<()> {
    let store = open_store()?;
    match args {
        UsageArgs::Stats { profile, by_key } => {
            if by_key {
                stats_by_key(&store, profile)
            } else {
                stats(&store, profile)
            }
        }
        UsageArgs::Recent { profile, limit } => recent(&store, profile, limit),
    }
}

fn resolve(
    store: &crate::storage::Store,
    profile: Option<String>,
) -> Result<crate::domain::Profile> {
    let name = match profile {
        Some(p) if !p.is_empty() => p,
        _ => Input::<String>::with_theme(&ColorfulTheme::default())
            .with_prompt("Profile name")
            .interact_text()?,
    };
    require_profile(store, &name)
}

fn stats(store: &crate::storage::Store, profile: Option<String>) -> Result<()> {
    let profile = resolve(store, profile)?;
    let rows = store.usage_stats(profile.id.as_str())?;
    if rows.is_empty() {
        println!("No usage recorded for {:?} yet.", profile.name);
        return Ok(());
    }
    println!("Usage for {:?} (per model):", profile.name);
    println!(
        "{:<28} {:>7} {:>9} {:>12} {:>10} {:>12} {:>10}",
        "MODEL", "CALLS", "FAILURES", "AVG LAT(ms)", "PROMPT", "COMPLETION", "EST $"
    );
    let mut total = 0.0;
    for s in rows {
        total += s.est_cost_usd;
        println!(
            "{:<28} {:>7} {:>9} {:>12.0} {:>10} {:>12} {:>10.4}",
            s.model_id,
            s.calls,
            s.failures,
            s.avg_latency_ms,
            s.prompt_tokens,
            s.completion_tokens,
            s.est_cost_usd
        );
    }
    println!("{:<28} {:>48} {:>10.4}", "TOTAL", "", total);
    println!("Tip: `route edit` → Economy + `route economy` to cut this bill 60-85%.");
    Ok(())
}

/// Per-key view: how a profile's traffic spread across the upstream keys behind
/// its routes, and how often each one was throttled.
fn stats_by_key(store: &crate::storage::Store, profile: Option<String>) -> Result<()> {
    let profile = resolve(store, profile)?;
    let rows = store.key_stats(profile.id.as_str())?;
    if rows.is_empty() {
        println!("No usage recorded for {:?} yet.", profile.name);
        return Ok(());
    }
    println!("Usage for {:?} (per key):", profile.name);
    println!(
        "{:<20} {:>18} {:>7} {:>9} {:>8} {:>12}",
        "KEY", "MASK", "CALLS", "FAILURES", "429s", "AVG LAT(ms)"
    );
    let total: i64 = rows.iter().map(|s| s.calls).sum();
    let throttled: i64 = rows.iter().map(|s| s.rate_limited).sum();
    for s in &rows {
        println!(
            "{:<20} {:>18} {:>7} {:>9} {:>8} {:>12.0}",
            s.provider_name,
            s.mask_name.as_deref().unwrap_or("-"),
            s.calls,
            s.failures,
            s.rate_limited,
            s.avg_latency_ms
        );
    }
    println!(
        "{:<20} {:>18} {:>7} {:>9} {:>8}",
        "TOTAL", "", total, "", ""
    );
    if throttled > 0 {
        println!(
            "Note: {throttled} request(s) came back rate limited. Keys that share \
             an egress IP share an upstream quota, so bind each key to its own \
             mask (`provider edit`) to spread the load; a key is parked for its \
             retry window (AGOS_RATE_LIMIT_COOLDOWN_SECS) after a 429."
        );
    }
    Ok(())
}

fn recent(store: &crate::storage::Store, profile: Option<String>, limit: u32) -> Result<()> {
    let profile = resolve(store, profile)?;
    let rows = store.list_usage(profile.id.as_str(), limit)?;
    if rows.is_empty() {
        println!("No usage recorded for {:?} yet.", profile.name);
        return Ok(());
    }
    println!("Last {} requests for {:?}:", rows.len(), profile.name);
    for r in rows {
        // `created_at` is stored as epoch milliseconds; `from_timestamp` wants seconds.
        let when = chrono::DateTime::from_timestamp_millis(r.created_at)
            .map(|dt| {
                dt.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_else(|| r.created_at.to_string());
        let outcome = match (r.success, r.status_code) {
            (true, Some(code)) => format!("ok ({code})"),
            (true, None) => "ok".to_string(),
            (false, Some(code)) => format!("failed ({code})"),
            (false, None) => "failed".to_string(),
        };
        let tokens = match (r.prompt_tokens, r.completion_tokens) {
            (Some(p), Some(c)) => format!("{p}/{c}"),
            _ => "-".to_string(),
        };
        println!(
            "  [{}] {} model={} request_id={} {} latency={}ms tokens={}",
            when,
            if r.streamed { "stream" } else { "direct" },
            r.model_id,
            r.request_id.as_deref().unwrap_or("-"),
            outcome,
            r.latency_ms,
            tokens
        );
        if let Some(err) = r.error_message.as_deref() {
            println!("        error: {err}");
        }
    }
    Ok(())
}

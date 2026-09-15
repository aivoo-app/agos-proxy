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
        UsageArgs::Stats { profile } => stats(&store, profile),
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

fn recent(store: &crate::storage::Store, profile: Option<String>, limit: u32) -> Result<()> {
    let profile = resolve(store, profile)?;
    let rows = store.list_usage(profile.id.as_str(), limit)?;
    if rows.is_empty() {
        println!("No usage recorded for {:?} yet.", profile.name);
        return Ok(());
    }
    println!("Last {} requests for {:?}:", rows.len(), profile.name);
    for r in rows {
        let when = chrono::DateTime::from_timestamp(r.created_at, 0)
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
            "  [{}] {} model={} {} latency={}ms tokens={}",
            when,
            if r.streamed { "stream" } else { "direct" },
            r.model_id,
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

//! Interactive chat for testing a configured route against real providers.
//!
//! `agos-proxy chat` is a wizard: it presents the configured profiles, proxies and
//! routes as selectable lists, lets you pick one of each, then opens an
//! interactive chat session routed through that chain exactly like an HTTP
//! request. Strategy reordering, capability filtering and priority failover
//! all apply, so you can validate a chain before wiring up an OpenAI SDK
//! client.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use clap::Parser;
use dialoguer::{theme::ColorfulTheme, Select};

use crate::adapter::outbound::forward_non_streaming;
use crate::cli::util::open_store;
use crate::domain::Profile;
use crate::router::{
    execute_with_failover, resolve_targets_with_strategy, RequestNeeds, RoutingState,
};
use crate::storage::Store;
use crate::translator::{content_text, ChatRequest, Message};

/// Arguments for `agos-proxy chat`. The wizards pick profile → proxy → route.
#[derive(Debug, Parser)]
pub struct ChatArgs {
    /// Run the conversation in a full-screen terminal UI instead of the
    /// line-by-line REPL.
    #[arg(long)]
    pub tui: bool,
}

/// Entry point for `agos-proxy chat`.
pub fn run(args: ChatArgs) -> Result<()> {
    let store = open_store()?;
    let theme = ColorfulTheme::default();

    // Surface the failover chain's warnings (attempt failed / timed out) in the
    // REPL so a silent chain is never mistaken for a hang. The TUI gets its own
    // tracing setup (or none) because raw log lines would corrupt its rendering.
    if !args.tui {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_target(false)
            .with_writer(std::io::stderr)
            .try_init();
    }

    let profile = select_profile(&store, &theme)?;
    let proxy = select_proxy(&store, &theme, &profile)?;
    let route = select_route(&store, &theme, &proxy)?;
    let model = format!("{}/{}", proxy.name, route.name);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let chain_len = store.route_entries(route.id)?.len();

    if args.tui {
        let session = super::tui::Session {
            store: Arc::new(store),
            profile,
            model: model.clone(),
            client,
            chain_len,
        };
        return super::tui::run(session);
    }

    println!();
    println!(
        "Starting chat on {model} (profile {:?}, {chain_len} model(s) in the fallback chain).",
        profile.name
    );
    println!("Type /help for commands, or Ctrl-D to exit.");
    println!();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        chat_session(
            Arc::new(store),
            client,
            RoutingState::default(),
            profile.id.clone(),
            model,
            chain_len,
            Duration::from_secs(60),
        )
        .await
    })?;
    Ok(())
}

/// Present the configured profiles and let the caller pick one.
fn select_profile(store: &Store, theme: &ColorfulTheme) -> Result<Profile> {
    let profiles = store.list_profiles()?;
    if profiles.is_empty() {
        bail!("no profiles configured; create one with `agos-proxy profile create` first");
    }
    let labels: Vec<String> = profiles
        .iter()
        .map(|p| {
            format!(
                "{} — {}",
                p.name,
                p.description.as_deref().unwrap_or("no description")
            )
        })
        .collect();
    let idx = Select::with_theme(theme)
        .with_prompt("Choose a profile")
        .items(&labels)
        .interact()?;
    Ok(profiles[idx].clone())
}

/// Present the proxies under a profile and let the caller pick one.
fn select_proxy(
    store: &Store,
    theme: &ColorfulTheme,
    profile: &Profile,
) -> Result<crate::domain::Proxy> {
    let proxies = store.list_proxies(profile.id.as_str())?;
    if proxies.is_empty() {
        bail!(
            "no proxies configured for {:?}; create one with `agos-proxy proxy create` first",
            profile.name
        );
    }
    let labels: Vec<String> = proxies
        .iter()
        .map(|p| {
            format!(
                "{} — {}",
                p.name,
                p.description.as_deref().unwrap_or("no description")
            )
        })
        .collect();
    let idx = Select::with_theme(theme)
        .with_prompt("Choose a proxy (provider)")
        .items(&labels)
        .interact()?;
    Ok(proxies[idx].clone())
}

/// Present the routes under a proxy and let the caller pick one.
fn select_route(
    store: &Store,
    theme: &ColorfulTheme,
    proxy: &crate::domain::Proxy,
) -> Result<crate::domain::Route> {
    let routes = store.list_routes(proxy.id)?;
    if routes.is_empty() {
        bail!(
            "no routes under proxy {:?}; create one with `agos-proxy route create` first",
            proxy.name
        );
    }
    let labels: Vec<String> = routes
        .iter()
        .map(|r| format!("{} ({:?})", r.name, r.strategy))
        .collect();
    let idx = Select::with_theme(theme)
        .with_prompt("Choose a route (model)")
        .items(&labels)
        .interact()?;
    Ok(routes[idx].clone())
}

/// Turn taken by the REPL after processing one line.
enum LineOutcome {
    /// Keep chatting.
    KeepGoing,
    /// Leave the loop.
    Quit,
}

/// Run the interactive conversation. History lives in memory for the session.
async fn chat_session(
    store: Arc<Store>,
    client: reqwest::Client,
    routing: RoutingState,
    profile_id: String,
    model: String,
    chain_len: usize,
    attempt_timeout: Duration,
) -> Result<()> {
    let mut history: Vec<Message> = Vec::new();
    let mut lines = std::io::stdin().lines();

    loop {
        eprint!("you> ");

        match lines.next() {
            None => break,
            Some(Err(e)) => {
                eprintln!("error reading input: {e}");
                break;
            }
            Some(Ok(line)) => {
                let text: &str = line.as_str().trim();
                if text.is_empty() {
                    continue;
                }
                // A leading '/' turns the line into a client-side command.
                if let Some(rest) = command_word(text) {
                    match handle_command(rest.as_str(), &mut history) {
                        LineOutcome::KeepGoing => continue,
                        LineOutcome::Quit => break,
                    }
                }

                history.push(Message::text("user", text.to_string()));

                eprintln!("thinking… (failover chain: {chain_len} model(s))");
                match send_turn(
                    &store,
                    &client,
                    &routing,
                    profile_id.as_str(),
                    model.as_str(),
                    history.clone(),
                    attempt_timeout,
                )
                .await
                {
                    Ok(reply) => {
                        history.push(Message::text("assistant", reply.clone()));
                        println!("assistant> {reply}");
                    }
                    Err(e) => {
                        eprintln!("(no reply — {e:#}; use /clear if the context feels stale)");
                    }
                }
            }
        }
    }

    eprintln!("Bye. {} message(s) exchanged.", history.len());
    Ok(())
}

/// Send one turn through the route's failover chain and return the assistant's
/// reply text.
pub(crate) async fn send_turn(
    store: &Arc<Store>,
    client: &reqwest::Client,
    routing: &RoutingState,
    profile_id: &str,
    model: &str,
    messages: Vec<Message>,
    attempt_timeout: Duration,
) -> Result<String> {
    let targets =
        resolve_targets_with_strategy(store, profile_id, model, RequestNeeds::default(), routing)
            .context("resolving route")?;
    if targets.is_empty() {
        bail!(
            "no healthy targets for {model}; check `agos-proxy route status` or wait for health recovery"
        );
    }

    let chat_req = ChatRequest {
        model: model.to_string(),
        messages,
        stream: false,
        extra: serde_json::Value::Null,
    };

    let bytes: Vec<u8> = execute_with_failover(store.clone(), targets, attempt_timeout, |target| {
        let client = client.clone();
        let req = chat_req.clone();
        async move { forward_non_streaming(&client, &target, &req).await }
    })
    .await?;

    // The reply is OpenAI-shaped regardless of which provider actually served it.
    let reply: serde_json::Value = serde_json::from_slice(&bytes).context("parsing reply")?;
    let content = reply
        .get("choices")
        .and_then(|a| a.as_array())
        .and_then(|a| a.first().and_then(|x| x.get("message")))
        .and_then(|msg| msg.get("content"));
    match content {
        Some(c) => Ok(content_text(c)),
        None => bail!("the provider returned no message content"),
    }
}

/// If `text` begins with a `/`, return everything after that leading slash as
/// an owned string. Returns `None` for ordinary chat lines.
fn command_word(text: &str) -> Option<String> {
    let mut it = text.chars();
    match it.next() {
        Some('/') => {
            let mut rest = String::new();
            for c in it {
                rest.push(c);
            }
            Some(rest)
        }
        _ => None,
    }
}

/// Handle a client-side command (the text after a leading '/').
fn handle_command(rest: &str, history: &mut Vec<Message>) -> LineOutcome {
    let cmd = rest.to_string();
    if cmd == "help" {
        println!("Commands:");
        println!("  /help      show this help");
        println!("  /clear     forget the conversation and start fresh");
        println!("  /quit      leave the session (also Ctrl-D)");
        LineOutcome::KeepGoing
    } else if cmd == "clear" || cmd == "reset" {
        history.clear();
        println!("Conversation cleared.");
        LineOutcome::KeepGoing
    } else if cmd == "quit" || cmd == "exit" || cmd == "bye" {
        LineOutcome::Quit
    } else {
        eprintln!("unknown command /{rest}; try /help");
        LineOutcome::KeepGoing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::domain::{ProviderKind, RoutingStrategy};
    use crate::storage::{NewProvider, Store};

    /// A mock upstream that returns a fixed assistant reply.
    async fn mock_upstream(port: u16) -> tokio::task::JoinHandle<()> {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(|| async {
                let body = serde_json::json!({
                    "id": "mock-1",
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": { "role": "assistant", "content": "hello from chat" },
                        "finish_reason": "stop"
                    }]
                });
                (axum::http::StatusCode::OK, axum::Json(body))
            }),
        );
        let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
            .await
            .expect("bind mock upstream");
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() })
    }

    #[tokio::test]
    async fn send_turn_routes_through_route_and_returns_reply() {
        let port = 19890;
        let base = format!("http://127.0.0.1:{port}");
        let _mock = mock_upstream(port).await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let store = Store::open_in_memory().expect("open store");
        let profile = store
            .create_profile("coder1", None, None)
            .expect("create profile");
        store
            .create_provider(
                &profile.id,
                NewProvider {
                    name: "mock".into(),
                    description: None,
                    base_url: base,
                    auth_token: "sk-mock".into(),
                    kind: ProviderKind::OpenAI,
                    extra_headers: BTreeMap::new(),
                },
            )
            .expect("create provider");
        let providers = store.list_providers(&profile.id).expect("list providers");
        let provider = providers[0].clone();
        let proxy = store
            .create_proxy(&profile.id, "prog", None)
            .expect("create proxy");
        let route = store
            .create_route(proxy.id, "r1", None, RoutingStrategy::Priority, None)
            .expect("create route");
        store
            .add_route_entry(route.id, provider.id, "m1", 1, 1.0, Default::default())
            .expect("add route entry");

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("build client");
        let store = Arc::new(store);
        let messages: Vec<Message> = vec![Message::text("user", "hi")];

        let reply = send_turn(
            &store,
            &client,
            &RoutingState::default(),
            profile.id.as_str(),
            "prog/r1",
            messages,
            Duration::from_secs(5),
        )
        .await
        .expect("send_turn succeeds");

        assert_eq!(reply, "hello from chat");
    }

    #[test]
    fn command_word_detects_leading_slash_only() {
        assert_eq!(command_word("/help").unwrap(), "help");
        assert_eq!(command_word("/quit").unwrap(), "quit");
        assert!(command_word("hello").is_none());
        assert!(command_word("").is_none());
    }
}

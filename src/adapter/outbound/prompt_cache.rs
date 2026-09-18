//! Provider-native prompt caching: mark up an already-translated upstream body
//! so the provider can bill a stable prefix at the cache-read rate.
//!
//! This runs *after* each adapter has produced its native body, which keeps the
//! translators pure and puts all the provider-specific cache dialects in one
//! place. Three rules govern everything here:
//!
//! 1. **Never break the request.** Every mutation is a pure add-on to a body we
//!    already built and validated; nothing existing is reordered or removed.
//!    A cache hint is an economy win, never a correctness requirement.
//! 2. **Never fight the client.** If the caller already supplied cache markers
//!    (`cache_control`, `prompt_cache_key`, `cachedContent`), we leave its
//!    choices alone — the client knows its own prefix reuse pattern best.
//! 3. **Never guess silently.** Caching is only applied when the route's
//!    `PromptCachePolicy` asks for it, because a cache *write* costs more than
//!    an uncached read and a route that rewrites its prompt every call would pay
//!    that premium for a cache it never reads.
//!
//! Provider dialects:
//!
//! - **Anthropic** — explicit `cache_control: {"type": "ephemeral"}` markers on
//!   content blocks. Marking a block caches everything *up to and including* it,
//!   so marking is about prefix boundaries, not individual blocks. We mark the
//!   system block (the long-lived operator instructions) and the last block of
//!   the final user turn (the conversation prefix), which is the pattern
//!   Anthropic documents for multi-turn caching.
//! - **OpenAI / OpenAI Responses** — `prompt_cache_key`, a routing hint that
//!   keeps requests sharing a prefix on the same cache shard. Best-effort and
//!   free: the provider caches automatically once a prefix crosses its minimum
//!   length. The key is a hash of the *prefix* (system prompt + first turn),
//!   never of the whole conversation, so all turns of one conversation land on
//!   the same shard while unrelated conversations do not.
//! - **Google** — caching is an explicit server-side resource (`cachedContent`)
//!   that must be created and named first, so there is nothing to inject on a
//!   plain request. A caller that has already created a cache passes
//!   `cachedContent` through untouched; managing that resource's lifecycle is a
//!   documented follow-up rather than something done behind the caller's back.

use serde_json::Value;

use crate::domain::{PromptCachePolicy, ProviderKind};

/// Apply the route's prompt-cache policy to a translated upstream body.
///
/// Returns the number of markers actually added, so an operator can confirm the
/// route is really being marked up.
pub fn apply(kind: ProviderKind, policy: PromptCachePolicy, body: &mut Value) -> usize {
    if !matches!(policy, PromptCachePolicy::Auto) {
        return 0;
    }
    match kind {
        ProviderKind::Anthropic => mark_anthropic(body),
        ProviderKind::OpenAI | ProviderKind::Custom | ProviderKind::OpenAIResponses => {
            mark_openai(body)
        }
        // Nothing to inject: `cachedContent` is an explicit resource the caller
        // must create first. Passing it through needs no help from us.
        ProviderKind::Google => 0,
    }
}

/// Stable routing-hint key derived from a body's cacheable prefix.
///
/// Derived from the prefix rather than the whole conversation so every turn of
/// one conversation shares a shard. Unrelated prompts differ in their very first
/// system/user content, so they hash apart.
fn prefix_key(body: &Value) -> Option<String> {
    let mut prefix = String::new();
    if let Some(system) = body.get("system") {
        prefix.push_str(&block_text(system));
    }
    if let Some(instructions) = body.get("instructions") {
        prefix.push_str(&block_text(instructions));
    }
    if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
        if let Some(first) = messages.first() {
            if let Some(content) = first.get("content") {
                prefix.push_str(&block_text(content));
            }
        }
    }
    if let Some(input) = body.get("input").and_then(|i| i.as_array()) {
        if let Some(first) = input.first() {
            prefix.push_str(&block_text(first));
        }
    }
    if prefix.trim().is_empty() {
        return None;
    }
    // FNV-1a: tiny, dependency-free, and stable across processes — a key that
    // changed per process would defeat the shard affinity it exists to create.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in prefix.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Some(format!("agos-{hash:016x}"))
}

/// Flatten a content value (string, block, or array of blocks) to its text.
fn block_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(block_text).collect::<Vec<_>>().join(""),
        Value::Object(obj) => ["text", "content", "instructions"]
            .iter()
            .filter_map(|k| obj.get(*k))
            .map(block_text)
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Add `prompt_cache_key` when the caller has not chosen one.
fn mark_openai(body: &mut Value) -> usize {
    if body.get("prompt_cache_key").is_some() {
        return 0;
    }
    let Some(key) = prefix_key(body) else {
        return 0;
    };
    if let Some(obj) = body.as_object_mut() {
        obj.insert("prompt_cache_key".to_string(), Value::String(key));
        return 1;
    }
    0
}

/// Whether the body already carries any client-supplied Anthropic marker.
fn has_cache_control(body: &Value) -> bool {
    fn scan(value: &Value) -> bool {
        match value {
            Value::Object(obj) => obj.contains_key("cache_control") || obj.values().any(scan),
            Value::Array(items) => items.iter().any(scan),
            _ => false,
        }
    }
    scan(body)
}

/// Mark the system block and the last text block of the final user turn.
fn mark_anthropic(body: &mut Value) -> usize {
    if has_cache_control(body) {
        return 0;
    }
    let mut added = 0;

    // Anthropic accepts `system` as either a string or a block array. Convert a
    // string into a one-element array so there is a block to mark; a request
    // without a system prompt skips this step rather than inventing one.
    if let Some(system) = body.get_mut("system") {
        if system.is_string() {
            let text = system.as_str().unwrap_or_default().to_string();
            *system = serde_json::json!([{ "type": "text", "text": text }]);
        }
        if let Some(last) = system
            .as_array_mut()
            .and_then(|blocks| blocks.last_mut())
            .and_then(|b| b.as_object_mut())
        {
            if !last.contains_key("cache_control") {
                last.insert("cache_control".to_string(), ephemeral());
                added += 1;
            }
        }
    }

    // The newest user turn is the growing edge of the conversation prefix:
    // everything before it is byte-stable across turns, which is exactly the
    // span worth caching.
    if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        if let Some(last_user) = messages
            .iter_mut()
            .rev()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        {
            // Normalize a plain-string turn into a one-block array so there is
            // something to mark, then mark the last text block: an image block
            // cannot carry `cache_control` on every upstream, and the text tail
            // is what advances between turns anyway.
            if let Some(content) = last_user.get_mut("content") {
                if content.is_string() {
                    let text = content.as_str().unwrap_or_default().to_string();
                    *content = serde_json::json!([{ "type": "text", "text": text }]);
                }
                if let Some(blocks) = content.as_array_mut() {
                    if let Some(idx) = blocks
                        .iter()
                        .rposition(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                    {
                        if let Some(obj) = blocks[idx].as_object_mut() {
                            if !obj.contains_key("cache_control") {
                                obj.insert("cache_control".to_string(), ephemeral());
                                added += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    added
}

/// The Anthropic marker value. `5m` is the default TTL and the only one
/// available without an opt-in beta header, so it is also the safest.
fn ephemeral() -> Value {
    serde_json::json!({ "type": "ephemeral" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn all_kinds() -> Vec<ProviderKind> {
        vec![
            ProviderKind::Anthropic,
            ProviderKind::OpenAI,
            ProviderKind::Custom,
            ProviderKind::Google,
            ProviderKind::OpenAIResponses,
        ]
    }

    #[test]
    fn off_policy_never_touches_the_body() {
        let original = json!({
            "model": "claude-3-5-sonnet",
            "system": "be terse",
            "messages": [{"role": "user", "content": "hi"}]
        });
        for kind in all_kinds() {
            let mut body = original.clone();
            assert_eq!(apply(kind, PromptCachePolicy::Off, &mut body), 0);
            assert_eq!(body, original, "{kind:?} must be untouched when off");
        }
    }

    #[test]
    fn anthropic_marks_system_and_last_user_turn() {
        let mut body = json!({
            "model": "claude-3-5-sonnet",
            "system": "long stable instructions",
            "messages": [
                {"role": "user", "content": "turn one"},
                {"role": "assistant", "content": "answer one"},
                {"role": "user", "content": "turn two"}
            ]
        });
        assert_eq!(
            apply(ProviderKind::Anthropic, PromptCachePolicy::Auto, &mut body),
            2
        );

        // The system prompt becomes a block array carrying the marker.
        assert_eq!(
            body["system"],
            json!([{
                "type": "text",
                "text": "long stable instructions",
                "cache_control": {"type": "ephemeral"}
            }])
        );
        // The marker lands on the newest user turn, never the earlier one.
        assert_eq!(body["messages"][0]["content"], json!("turn one"));
        assert_eq!(
            body["messages"][2]["content"],
            json!([{"type": "text", "text": "turn two", "cache_control": {"type": "ephemeral"}}])
        );
    }

    #[test]
    fn anthropic_marks_the_text_tail_of_a_multimodal_turn() {
        let mut body = json!({
            "model": "claude-3-5-sonnet",
            "system": "be terse",
            "messages": [{"role": "user", "content": [
                {"type": "image", "source": {"type": "url", "url": "https://x/i.png"}},
                {"type": "text", "text": "what is this"}
            ]}]
        });
        assert_eq!(
            apply(ProviderKind::Anthropic, PromptCachePolicy::Auto, &mut body),
            2
        );
        // Images must never gain a marker (not universally accepted)...
        assert!(body["messages"][0]["content"][0]
            .get("cache_control")
            .is_none());
        // ...while the text block, which is the advancing edge, does.
        assert_eq!(
            body["messages"][0]["content"][1]["cache_control"],
            json!({"type": "ephemeral"})
        );
    }

    #[test]
    fn client_supplied_markers_are_never_duplicated() {
        let system = json!([{"type": "text", "text": "s", "cache_control": {"type": "ephemeral"}}]);
        let mut body = json!({
            "model": "claude-3-5-sonnet",
            "system": system.clone(),
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert_eq!(
            apply(ProviderKind::Anthropic, PromptCachePolicy::Auto, &mut body),
            0
        );
        // The caller's block array is left exactly as it was, and its plain
        // string turn is not rewritten either: the client owns its markers.
        assert_eq!(body["system"], system);
        assert_eq!(body["messages"][0]["content"], json!("hi"));
    }

    #[test]
    fn anthropic_without_a_system_prompt_still_marks_the_user_turn() {
        let mut body = json!({
            "model": "claude-3-5-sonnet",
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert_eq!(
            apply(ProviderKind::Anthropic, PromptCachePolicy::Auto, &mut body),
            1
        );
        assert!(body.get("system").is_none(), "no system prompt is invented");
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
    }

    #[test]
    fn anthropic_with_only_an_assistant_turn_adds_nothing() {
        // No user turn means no growing edge to mark; the body must survive.
        let mut body = json!({
            "model": "claude-3-5-sonnet",
            "messages": [{"role": "assistant", "content": "prior answer"}]
        });
        assert_eq!(
            apply(ProviderKind::Anthropic, PromptCachePolicy::Auto, &mut body),
            0
        );
        assert_eq!(body["messages"][0]["content"], json!("prior answer"));
    }

    #[test]
    fn openai_gets_a_stable_key_derived_from_the_prefix() {
        let body_for = |tail: &str| {
            json!({
                "model": "gpt-4o",
                "messages": [
                    {"role": "system", "content": "you are a router"},
                    {"role": "user", "content": "explain"},
                    {"role": "assistant", "content": tail},
                    {"role": "user", "content": tail}
                ]
            })
        };
        let mut a = body_for("continuing the same conversation");
        let mut b = body_for("a completely different follow-up");
        assert_eq!(apply(ProviderKind::OpenAI, PromptCachePolicy::Auto, &mut a), 1);
        assert_eq!(apply(ProviderKind::OpenAI, PromptCachePolicy::Auto, &mut b), 1);
        // Same prefix, different tail: one shard.
        assert_eq!(a["prompt_cache_key"], b["prompt_cache_key"]);
        assert!(a["prompt_cache_key"].as_str().unwrap().starts_with("agos-"));

        // A different operator prompt is a different prefix, hence a new shard.
        let mut other = json!({
            "model": "gpt-4o",
            "messages": [{"role": "system", "content": "a different operator prompt"}]
        });
        assert_eq!(
            apply(ProviderKind::OpenAI, PromptCachePolicy::Auto, &mut other),
            1
        );
        assert_ne!(other["prompt_cache_key"], a["prompt_cache_key"]);
    }

    #[test]
    fn openai_keeps_a_client_supplied_key_and_skips_empty_prefixes() {
        let mut body = json!({
            "model": "gpt-4o",
            "prompt_cache_key": "client-chose-this",
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert_eq!(apply(ProviderKind::OpenAI, PromptCachePolicy::Auto, &mut body), 0);
        assert_eq!(body["prompt_cache_key"], json!("client-chose-this"));

        // Nothing but a model name: there is no prefix to key on.
        let mut empty = json!({ "model": "gpt-4o", "messages": [] });
        assert_eq!(apply(ProviderKind::OpenAI, PromptCachePolicy::Auto, &mut empty), 0);
        assert!(empty.get("prompt_cache_key").is_none());
    }

    #[test]
    fn responses_bodies_are_keyed_from_their_instructions() {
        let mut body = json!({
            "model": "gpt-4o",
            "instructions": "be concise",
            "input": [{"role": "user", "content": "hello"}]
        });
        assert_eq!(
            apply(
                ProviderKind::OpenAIResponses,
                PromptCachePolicy::Auto,
                &mut body
            ),
            1
        );
        assert!(body["prompt_cache_key"].as_str().is_some());
    }

    #[test]
    fn google_requests_pass_through_untouched() {
        let mut body = json!({
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "cachedContent": "caches/abc"
        });
        let before = body.clone();
        assert_eq!(apply(ProviderKind::Google, PromptCachePolicy::Auto, &mut body), 0);
        assert_eq!(body, before);
    }
}

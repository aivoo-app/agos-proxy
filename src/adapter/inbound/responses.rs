//! OpenAI Responses API inbound adapter — the surface the Codex CLI speaks.
//!
//! Serves `POST /codex/v1/responses`. Codex removed its `wire_api = "chat"`
//! mode (`CHAT_WIRE_API_REMOVED_ERROR` upstream), so the Responses API is the
//! only dialect a current Codex client speaks, and it always streams.
//!
//! The shape differs from chat completions in three ways this adapter bridges:
//!
//! - the request carries `instructions` plus an `input` item list rather than
//!   `messages`, and `input` items are internally tagged (`message`,
//!   `function_call`, `function_call_output`, ...);
//! - its tools are **flat** (`{type, name, description, parameters}`) rather
//!   than nested under a `function` key, and it has a freeform `custom` tool
//!   kind that chat completions has no equivalent for;
//! - the response is a `response` object with typed `output` items, delivered
//!   over SSE through events such as `response.output_text.delta` and a
//!   mandatory terminal `response.completed`.
//!
//! Codex only reads the JSON `type` field of each `data:` frame, and it fails
//! the turn if the stream closes before `response.completed`. Both facts drive
//! the renderer below.

use serde_json::{json, Map, Value};
use tracing::{debug, warn};

use crate::adapter::inbound::{InboundAdapter, StreamRenderer};
use crate::adapter::ApiKind;
use crate::translator::{
    CanonicalResponse, ChatRequest, Message, StreamEvent, ToolCall, ToolCallDelta,
};

/// The `extra` key holding Responses-only request fields that have no
/// chat-completions equivalent. The outbound adapters strip it before
/// forwarding, so a strict provider never sees these parameters.
const RESPONSES_KEY: &str = "agos_responses";

/// Responses-native inbound surface.
pub struct ResponsesAdapter;

/// Build the canonical content for a Responses `message` item.
///
/// Plain text becomes a JSON string so it is not mistaken for vision content
/// downstream; an item carrying images becomes the OpenAI parts array, which is
/// what the capability filter looks for.
fn message_content(content: Option<&Value>) -> Value {
    // The Responses wire format allows `content` to be a plain string.
    if let Some(s) = content.and_then(|c| c.as_str()) {
        return Value::String(s.to_string());
    }
    let Some(parts) = content.and_then(|c| c.as_array()) else {
        return Value::String(String::new());
    };

    let has_image = parts
        .iter()
        .any(|p| p.get("type").and_then(|t| t.as_str()) == Some("input_image"));

    if !has_image {
        return Value::String(join_text_parts(parts));
    }

    let openai_parts: Vec<Value> = parts
        .iter()
        .filter_map(|part| match part.get("type").and_then(|t| t.as_str()) {
            Some("input_text" | "output_text" | "text") => Some(json!({
                "type": "text",
                "text": part.get("text").and_then(|t| t.as_str()).unwrap_or_default(),
            })),
            Some("input_image") => Some(json!({
                "type": "image_url",
                "image_url": {
                    "url": part.get("image_url").and_then(|u| u.as_str()).unwrap_or_default(),
                },
            })),
            _ => None,
        })
        .collect();
    Value::Array(openai_parts)
}

/// Join the text of a Responses content-part array with newlines.
fn join_text_parts(parts: &[Value]) -> String {
    parts
        .iter()
        .filter(|p| {
            matches!(
                p.get("type").and_then(|t| t.as_str()),
                Some("input_text" | "output_text" | "text")
            )
        })
        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Coerce a Responses `arguments` value into the JSON string the wire format
/// uses. Codex always sends a string; a parsed object is re-serialized.
fn arguments_to_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => "{}".to_string(),
        Some(other) => other.to_string(),
    }
}

/// Render a tool result's `output`, which is either a plain string or an array
/// of content items.
fn output_content(output: Option<&Value>) -> Value {
    match output {
        Some(Value::String(s)) => Value::String(s.clone()),
        Some(Value::Array(parts)) => Value::String(join_text_parts(parts)),
        Some(other) => Value::String(other.to_string()),
        None => Value::String(String::new()),
    }
}

/// Append the canonical messages for one `input` item.
///
/// Returns `false` for items this surface cannot express as a chat-completions
/// message (reasoning items, server-side tool calls); the caller counts those
/// so it can log one summary line instead of one per item.
fn push_input_item(item: &Value, messages: &mut Vec<Message>) -> bool {
    let kind = item
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("message");
    match kind {
        "message" => {
            let role = item.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            messages.push(Message::new(role, message_content(item.get("content"))));
            true
        }
        // A freeform `custom` tool call and a function call both become a
        // chat-completions function tool call; the reverse mapping happens on
        // the response side.
        "function_call" | "custom_tool_call" => {
            let encoded = tool_call_json(item, kind);
            let appended = messages
                .last_mut()
                .filter(|m| m.role == "assistant" && m.extra.get("tool_calls").is_some())
                .and_then(|msg| msg.extra.get_mut("tool_calls"))
                .and_then(|calls| calls.as_array_mut())
                .map(|calls| {
                    // Parallel calls from one turn belong in a single message.
                    calls.push(encoded.clone());
                });
            if appended.is_none() {
                let mut msg = Message::new("assistant", Value::Null);
                msg.extra = json!({ "tool_calls": [encoded] });
                messages.push(msg);
            }
            true
        }
        "function_call_output" | "custom_tool_call_output" => {
            let mut msg = Message::new("tool", output_content(item.get("output")));
            msg.extra = json!({
                "tool_call_id": item.get("call_id").and_then(|c| c.as_str()).unwrap_or_default(),
            });
            messages.push(msg);
            true
        }
        _ => false,
    }
}

/// Encode a Responses call item as one chat-completions `tool_calls` entry.
fn tool_call_json(item: &Value, kind: &str) -> Value {
    let call_id = item
        .get("call_id")
        .and_then(|c| c.as_str())
        .unwrap_or_default();
    let id = item
        .get("id")
        .and_then(|i| i.as_str())
        .filter(|i| !i.is_empty())
        .unwrap_or(call_id);
    let arguments = if kind == "custom_tool_call" {
        // The freeform input is a raw grammar-conforming string.
        match item.get("input") {
            Some(Value::String(s)) => s.clone(),
            other => arguments_to_string(other),
        }
    } else {
        arguments_to_string(item.get("arguments"))
    };
    json!({
        "id": id,
        "type": "function",
        "function": {
            "name": item.get("name").and_then(|n| n.as_str()).unwrap_or_default(),
            "arguments": arguments,
        },
    })
}

/// Convert one Responses tool definition into chat-completions tools.
///
/// Returns an empty vector for kinds a chat-completions provider cannot serve
/// (`web_search`, `tool_search`); the caller warns about those. A `namespace`
/// tool is flattened into one entry per inner tool, named `namespace.tool`.
fn convert_tool(tool: &Value) -> Vec<Value> {
    match tool.get("type").and_then(|t| t.as_str()) {
        Some("function") => vec![json!({
            "type": "function",
            "function": {
                "name": tool.get("name").and_then(|n| n.as_str()).unwrap_or_default(),
                "description": tool.get("description").and_then(|d| d.as_str()).unwrap_or_default(),
                "parameters": tool
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object" })),
                "strict": tool.get("strict").and_then(|s| s.as_bool()).unwrap_or(false),
            },
        })],
        // A freeform tool has no chat-completions equivalent: it becomes a
        // single-string parameter, with the expected syntax carried in the
        // description so the model still produces conforming input.
        Some("custom") => {
            let definition = tool
                .pointer("/format/definition")
                .and_then(|d| d.as_str())
                .unwrap_or_default();
            let description = tool
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or_default();
            let description = if definition.is_empty() {
                description.to_string()
            } else {
                format!("{description}\n\nInput grammar:\n{definition}")
            };
            vec![json!({
                "type": "function",
                "function": {
                    "name": tool.get("name").and_then(|n| n.as_str()).unwrap_or_default(),
                    "description": description,
                    "parameters": {
                        "type": "object",
                        "properties": { "input": { "type": "string" } },
                        "required": ["input"],
                        "additionalProperties": false,
                    },
                },
            })]
        }
        Some("namespace") => {
            let namespace = tool
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default();
            tool.get("tools")
                .and_then(|t| t.as_array())
                .map(|inner| {
                    inner
                        .iter()
                        .flat_map(convert_tool)
                        .map(|mut entry| {
                            let name = entry
                                .pointer("/function/name")
                                .and_then(|n| n.as_str())
                                .unwrap_or_default()
                                .to_string();
                            entry["function"]["name"] =
                                Value::String(format!("{namespace}.{name}"));
                            entry
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// Translate the Responses `tools` array, warning once about any kind that has
/// no chat-completions equivalent.
fn convert_tools(tools: &[Value]) -> Vec<Value> {
    let mut converted = Vec::new();
    let mut dropped = Vec::new();
    for tool in tools {
        let kind = tool.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let entries = convert_tool(tool);
        if entries.is_empty() {
            dropped.push(kind.to_string());
        } else {
            converted.extend(entries);
        }
    }
    if !dropped.is_empty() {
        warn!(
            dropped = %dropped.join(", "),
            "dropping Responses tools with no chat-completions equivalent; \
             the upstream provider cannot serve them"
        );
    }
    converted
}

/// Request fields the Responses API defines that have no chat-completions
/// equivalent. They are parked under [`RESPONSES_KEY`] rather than forwarded,
/// because a strict provider rejects unknown parameters.
const RESPONSES_ONLY_FIELDS: [&str; 10] = [
    "store",
    "include",
    "reasoning",
    "prompt_cache_key",
    "stream_options",
    "service_tier",
    "client_metadata",
    "text",
    "access_programs",
    "max_output_tokens",
];

impl InboundAdapter for ResponsesAdapter {
    fn kind(&self) -> ApiKind {
        ApiKind::Responses
    }

    fn parse_request(&self, body: &Value) -> anyhow::Result<ChatRequest> {
        let model = body
            .get("model")
            .and_then(|m| m.as_str())
            .filter(|m| !m.is_empty())
            .ok_or_else(|| anyhow::anyhow!("`model` is required"))?
            .to_string();

        let mut messages = Vec::new();
        // Codex sends its base instructions here; they are the system prompt.
        if let Some(instructions) = body.get("instructions").and_then(|i| i.as_str()) {
            if !instructions.trim().is_empty() {
                messages.push(Message::text("system", instructions));
            }
        }

        let mut skipped = 0usize;
        match body.get("input") {
            // The API also accepts a bare string as the whole input.
            Some(Value::String(text)) => messages.push(Message::text("user", text.clone())),
            Some(Value::Array(items)) => {
                for item in items {
                    if !push_input_item(item, &mut messages) {
                        skipped += 1;
                    }
                }
            }
            _ => {}
        }
        if skipped > 0 {
            debug!(
                skipped,
                "skipping Responses input items with no chat-completions equivalent \
                 (reasoning items and server-side tool calls are not replayable here)"
            );
        }

        let mut extra = Map::new();
        if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
            let converted = convert_tools(tools);
            if !converted.is_empty() {
                extra.insert("tools".to_string(), Value::Array(converted));
            }
        }
        for key in ["tool_choice", "parallel_tool_calls", "temperature", "top_p"] {
            if let Some(value) = body.get(key) {
                extra.insert(key.to_string(), value.clone());
            }
        }
        // `max_output_tokens` is the Responses spelling of `max_tokens`.
        if let Some(max) = body.get("max_output_tokens") {
            extra.insert("max_tokens".to_string(), max.clone());
        }

        // Responses-only fields are parked out of the way for the outbound
        // adapters to strip.
        let mut reserved = Map::new();
        for key in RESPONSES_ONLY_FIELDS {
            if let Some(value) = body.get(key) {
                reserved.insert(key.to_string(), value.clone());
            }
        }
        if !reserved.is_empty() {
            extra.insert(RESPONSES_KEY.to_string(), Value::Object(reserved));
        }

        Ok(ChatRequest {
            model,
            messages,
            // Codex always streams; a caller that omits `stream` is a Codex
            // client, so streaming is the safe default on this surface.
            stream: body.get("stream").and_then(|s| s.as_bool()).unwrap_or(true),
            extra: Value::Object(extra),
        })
    }
    fn render_response(&self, resp: &CanonicalResponse) -> Value {
        let id = response_id(&resp.id);
        let mut output = Vec::new();
        if !resp.text.is_empty() {
            output.push(message_item(&resp.text, &id));
        }
        for (index, call) in resp.tool_calls.iter().enumerate() {
            output.push(function_call_item(call, index));
        }

        json!({
            "id": id,
            "object": "response",
            "created_at": now_secs(),
            "status": "completed",
            "model": resp.model,
            "output": output,
            "parallel_tool_calls": true,
            "tool_choice": "auto",
            "tools": [],
            "usage": usage_json(resp.prompt_tokens, resp.completion_tokens),
        })
    }

    fn render_stream_event(&self, ev: &StreamEvent, id: &str) -> Option<String> {
        // Best-effort single-event rendering, with no accumulated state. A
        // caller that wants the full Responses event sequence must use
        // [`crate::adapter::Registry::stream_renderer`], which owns the state
        // that the terminal events require.
        ResponsesRenderer::default().render(ev, id)
    }

    fn stream_end_marker(&self) -> Option<String> {
        // The Responses stream is terminated by `response.completed`, which the
        // renderer emits. There is no `data: [DONE]` sentinel, and Codex
        // ignores the `event:` line entirely.
        None
    }
}

fn response_id(id: &str) -> String {
    if id.starts_with("resp_") {
        id.to_string()
    } else if id.is_empty() {
        "resp_agos".to_string()
    } else {
        format!("resp_{id}")
    }
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The `usage` object Codex reads from `response.completed`.
fn usage_json(input_tokens: u64, output_tokens: u64) -> Value {
    json!({
        "input_tokens": input_tokens,
        "input_tokens_details": { "cached_tokens": 0 },
        "output_tokens": output_tokens,
        "output_tokens_details": { "reasoning_tokens": 0 },
        "total_tokens": input_tokens + output_tokens,
    })
}

/// A completed assistant `message` output item, with no annotations.
fn message_item(text: &str, response_id: &str) -> Value {
    json!({
        "type": "message",
        "id": format!("msg_{response_id}"),
        "status": "completed",
        "role": "assistant",
        "content": [{ "type": "output_text", "text": text, "annotations": [] }],
    })
}

/// A `function_call` output item. `arguments` is a JSON *string*, which is what
/// the wire format specifies and what Codex expects to parse.
fn function_call_item(call: &ToolCall, index: usize) -> Value {
    let id = if call.id.is_empty() {
        format!("fc_{index}")
    } else {
        call.id.clone()
    };
    let call_id = if call.call_id.is_empty() {
        id.clone()
    } else {
        call.call_id.clone()
    };
    json!({
        "type": "function_call",
        "id": id,
        "call_id": call_id,
        "name": call.name,
        "arguments": call.arguments,
        "status": "completed",
    })
}

/// Frame one Responses SSE event. Codex reads only the JSON `type`, but the
/// `event:` line is part of the format and other Responses clients use it.
fn sse(event: &str, payload: &Value) -> String {
    format!("event: {event}\ndata: {payload}\n\n")
}

/// The Responses-API [`StreamRenderer`].
///
/// Responses cannot emit its terminal events until the turn is complete:
/// `response.output_item.done` carries the *accumulated* text and tool-call
/// arguments, and `response.completed` closes the turn. This renderer owns that
/// per-stream state, which is exactly why it is a separate type rather than a
/// method on the stateless adapter.
///
/// The sequence a Codex client needs:
///
/// ```text
/// response.created
/// response.output_text.delta *    (live text)
/// response.output_item.done *     (the final message, then one per call)
/// response.completed              (response.id + usage; ends the turn)
/// ```
#[derive(Default)]
pub struct ResponsesRenderer {
    response_id: String,
    created: bool,
    text: String,
    calls: Vec<ToolCall>,
    prompt_tokens: u64,
    completion_tokens: u64,
    finished: bool,
}

impl ResponsesRenderer {
    fn message_item_id(&self) -> String {
        format!("msg_{}", self.response_id)
    }

    /// The opening event, which tells the client the response id.
    fn created_frame(&self) -> String {
        sse(
            "response.created",
            &json!({
                "type": "response.created",
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "created_at": now_secs(),
                    "status": "in_progress",
                    "output": [],
                },
            }),
        )
    }

    /// Fold one streamed fragment into the accumulated call at its index.
    fn absorb(&mut self, delta: &ToolCallDelta) {
        let index = delta.index as usize;
        if self.calls.len() <= index {
            self.calls.resize(index + 1, ToolCall::default());
        }
        let call = &mut self.calls[index];
        if let Some(id) = delta.id.as_ref().filter(|i| !i.is_empty()) {
            call.id = id.clone();
        }
        if let Some(call_id) = delta.call_id.as_ref().filter(|i| !i.is_empty()) {
            call.call_id = call_id.clone();
        }
        if let Some(name) = delta.name.as_ref().filter(|n| !n.is_empty()) {
            call.name = name.clone();
        }
        if let Some(arguments) = delta.arguments_delta.as_ref() {
            call.arguments.push_str(arguments);
        }
    }
}

impl ResponsesRenderer {
    /// The closing burst: the finished items, then `response.completed`.
    ///
    /// This is mandatory — Codex fails the turn with "stream closed before
    /// response.completed" if the stream ends without it.
    fn terminal_frames(&mut self) -> String {
        self.finished = true;
        let mut frames = String::new();
        let mut output_index = 0;

        if !self.text.is_empty() {
            frames.push_str(&sse(
                "response.output_item.done",
                &json!({
                    "type": "response.output_item.done",
                    "output_index": output_index,
                    "item": message_item(&self.text, &self.response_id),
                }),
            ));
            output_index += 1;
        }

        for (position, call) in self.calls.iter().enumerate() {
            // A fragment that never produced a name cannot be dispatched.
            if call.name.is_empty() {
                continue;
            }
            frames.push_str(&sse(
                "response.output_item.done",
                &json!({
                    "type": "response.output_item.done",
                    "output_index": output_index + position,
                    "item": function_call_item(call, position),
                }),
            ));
        }

        frames.push_str(&sse(
            "response.completed",
            &json!({
                "type": "response.completed",
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "created_at": now_secs(),
                    "status": "completed",
                    "usage": usage_json(self.prompt_tokens, self.completion_tokens),
                },
            }),
        ));
        frames
    }
}

impl StreamRenderer for ResponsesRenderer {
    fn render(&mut self, ev: &StreamEvent, id: &str) -> Option<String> {
        if self.finished {
            return None;
        }
        if self.response_id.is_empty() {
            self.response_id = response_id(id);
        }

        let mut frames = String::new();
        if !self.created {
            self.created = true;
            frames.push_str(&self.created_frame());
        }

        if let Some(prompt) = ev.prompt_tokens {
            self.prompt_tokens = prompt;
        }
        if let Some(completion) = ev.completion_tokens {
            self.completion_tokens = completion;
        }
        for delta in &ev.tool_call_deltas {
            self.absorb(delta);
        }

        if !ev.delta.is_empty() {
            self.text.push_str(&ev.delta);
            frames.push_str(&sse(
                "response.output_text.delta",
                &json!({
                    "type": "response.output_text.delta",
                    "item_id": self.message_item_id(),
                    "output_index": 0,
                    "content_index": 0,
                    "delta": ev.delta,
                }),
            ));
        }

        if ev.finish_reason.is_some() || ev.done {
            frames.push_str(&self.terminal_frames());
        }

        if frames.is_empty() {
            None
        } else {
            Some(frames)
        }
    }

    fn finish(&mut self, id: &str) -> Option<String> {
        if self.finished {
            return None;
        }
        if self.response_id.is_empty() {
            self.response_id = response_id(id);
        }
        let mut frames = String::new();
        if !self.created {
            self.created = true;
            frames.push_str(&self.created_frame());
        }
        frames.push_str(&self.terminal_frames());
        Some(frames)
    }
}

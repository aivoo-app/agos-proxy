//! Master adapter module.
//!
//! AGOS Proxy can speak several *native* APIs at once. Each inbound surface
//! (OpenAI, Anthropic, Google, OpenAI Responses) is implemented by an
//! [`inbound::InboundAdapter`] that parses a native request into the canonical
//! [`ChatRequest`] and renders canonical results back in its own wire format.
//! Each upstream provider kind gets an outbound adapter in [`outbound`] doing
//! the mirror image.
//!
//! This module is the single point of allocation: it maps an inbound request to
//! the correct inbound adapter (by [`ApiKind`]) and hands the canonical request
//! to the router / outbound translator pipeline, so an Anthropic client and an
//! OpenAI client can share the same routes with no per-surface special-casing in
//! the server layer.

pub mod inbound;
pub mod outbound;

use std::collections::HashMap;

use crate::translator::{CanonicalResponse, ChatRequest, StreamEvent};

pub use inbound::{InboundAdapter, StatelessRenderer, StreamRenderer};

/// The native API surface an inbound request speaks. Each variant maps to a
/// namespaced URL prefix and an [`InboundAdapter`] implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApiKind {
    OpenAI,
    Anthropic,
    Google,
    /// The OpenAI *Responses* API, served under `/codex` for the Codex CLI.
    ///
    /// Codex removed its `wire_api = "chat"` mode, so the Responses API is the
    /// only dialect a current Codex client speaks. See
    /// [`crate::adapter::inbound::responses`].
    Responses,
}

impl ApiKind {
    /// The leading path segment used to namespace this surface.
    pub fn prefix(self) -> &'static str {
        match self {
            ApiKind::OpenAI => "/openai",
            ApiKind::Anthropic => "/anthropic",
            ApiKind::Google => "/google",
            ApiKind::Responses => "/codex",
        }
    }

    /// Map a request path to the surface it belongs to, or `None` for the
    /// legacy unprefixed routes (which are served by the OpenAI surface).
    pub fn from_path(path: &str) -> Option<ApiKind> {
        let seg = path.trim_start_matches('/').split('/').next().unwrap_or("");
        match seg {
            "openai" => Some(ApiKind::OpenAI),
            "anthropic" => Some(ApiKind::Anthropic),
            "google" => Some(ApiKind::Google),
            "codex" => Some(ApiKind::Responses),
            _ => None,
        }
    }
}

/// The master registry of inbound adapters, built once and shared across all
/// requests via [`crate::server::handlers::AppState`].
#[derive(Clone)]
pub struct Registry {
    inbound: HashMap<ApiKind, &'static dyn InboundAdapter>,
}

impl Default for Registry {
    /// Build the default registry containing every inbound adapter.
    fn default() -> Self {
        let mut registry = Registry {
            inbound: HashMap::new(),
        };
        registry.register(&inbound::openai::OpenAiAdapter);
        registry.register(&inbound::anthropic::AnthropicAdapter);
        registry.register(&inbound::google::GoogleAdapter);
        registry.register(&inbound::responses::ResponsesAdapter);
        registry
    }
}

impl Registry {
    fn register(&mut self, adapter: &'static dyn InboundAdapter) {
        self.inbound.insert(adapter.kind(), adapter);
    }

    /// Look up the inbound adapter for a surface, defaulting to the OpenAI
    /// adapter for unknown kinds.
    pub fn inbound(&self, kind: ApiKind) -> &'static dyn InboundAdapter {
        self.inbound
            .get(&kind)
            .copied()
            .unwrap_or(&inbound::OpenAiAdapter)
    }
}

impl Registry {
    /// Parse a native request body into a canonical [`ChatRequest`].
    pub fn parse_request(
        &self,
        kind: ApiKind,
        body: &serde_json::Value,
    ) -> anyhow::Result<ChatRequest> {
        self.inbound(kind).parse_request(body)
    }

    /// Render a canonical response as native JSON on the given surface.
    pub fn render_response(
        &self,
        kind: ApiKind,
        resp: &CanonicalResponse,
    ) -> anyhow::Result<serde_json::Value> {
        self.inbound(kind).render_response(resp)
    }

    /// Render one canonical stream event as a native SSE frame on the surface.
    pub fn render_stream_event(&self, kind: ApiKind, ev: &StreamEvent, id: &str) -> Option<String> {
        self.inbound(kind).render_stream_event(ev, id)
    }

    /// Create the per-stream SSE renderer for a surface.
    ///
    /// The Responses API needs state that outlives a single event (accumulated
    /// text and tool-call arguments) to emit its terminal
    /// `response.output_item.done` items, so it supplies its own renderer.
    /// Every other surface is stateless and uses the default. This is the one
    /// place that knows which surface is stateful.
    pub fn stream_renderer(&self, kind: ApiKind) -> Box<dyn StreamRenderer> {
        match kind {
            ApiKind::Responses => Box::new(inbound::responses::ResponsesRenderer::default()),
            _ => Box::new(StatelessRenderer::new(self.inbound(kind))),
        }
    }
}

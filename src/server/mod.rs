//! The HTTP surface exposed to callers.
//!
//! AGOS Proxy speaks the OpenAI-compatible API — `/v1/chat/completions`,
//! `/v1/completions` and `/v1/models` — so an existing OpenAI SDK client can be
//! pointed at this server unchanged. This module builds the router and binds the
//! listening socket.
//!
//! The surface is wired up during the MVP milestone; nothing is served yet.

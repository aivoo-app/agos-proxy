//! Core data model for AGOS Proxy.
//!
//! Two parallel hierarchies hang off a shared account — the [`Profile`]:
//!
//! ```text
//! Profile
//!   ├── Providers   (how we talk to the outside world: base_url + credentials)
//!   └── Proxies     (what the outside world is allowed to ask for)
//!         └── Route ── an ordered fallback chain of models
//! ```
//!
//! A provider describes *how* a call is made. A proxy and its routes describe
//! *what* callers may request (a route never exposes a single model — it exposes
//! an ordered chain). This separation is what makes automatic failover possible:
//! the caller names a route, and AGOS Proxy decides which provider/model actually
//! answers.

pub mod model;

pub use model::{
    ModelStatus, Profile, Provider, ProviderKind, Proxy, Route, RouteCapabilities, RouteEntry,
    RoutingStrategy, UsageRecord, UsageStats,
};

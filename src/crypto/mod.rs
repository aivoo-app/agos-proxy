//! Handling of secrets at rest.
//!
//! Provider tokens are encrypted before they reach disk, keyed off a local
//! master key that is itself derived from an optional profile password (argon2).
//! Keeping this in its own module means token handling stays in one place and is
//! easy to audit and test in isolation.

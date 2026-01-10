//! Per-profile request rate limiting.
//!
//! A sliding one-minute window is kept per profile id: each accepted request
//! records a timestamp, timestamps older than the window are evicted, and a
//! request is refused once the recorded count reaches the profile's
//! requests-per-minute ceiling. The limiter lives entirely in memory — it is
//! per-process by design, since the proxy is a single binary.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How far back the sliding window reaches.
const WINDOW: Duration = Duration::from_secs(60);

/// Sliding-window rate limiter keyed by profile id.
#[derive(Default)]
pub struct RateLimiter {
    windows: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a hit for `key`. Returns `false` when the caller has already
    /// reached `rpm_limit` requests inside the current window and should be
    /// refused; the refused request is *not* recorded.
    pub fn check(&self, key: &str, rpm_limit: i64) -> bool {
        let now = Instant::now();
        let mut windows = self.windows.lock().expect("rate limiter mutex poisoned");
        let window = windows.entry(key.to_string()).or_default();
        while let Some(front) = window.front() {
            if now.duration_since(*front) >= WINDOW {
                window.pop_front();
            } else {
                break;
            }
        }
        if window.len() as i64 >= rpm_limit {
            return false;
        }
        window.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_up_to_limit_then_refuses() {
        let limiter = RateLimiter::new();
        for i in 0..3 {
            assert!(limiter.check("p1", 3), "request {i} should pass");
        }
        assert!(!limiter.check("p1", 3), "4th request should be refused");
        // Refused requests are not recorded, so the count stays at the limit.
        assert!(!limiter.check("p1", 3), "still over the limit");
    }

    #[test]
    fn keys_are_independent() {
        let limiter = RateLimiter::new();
        for _ in 0..5 {
            assert!(limiter.check("a", 5));
        }
        assert!(!limiter.check("a", 5));
        // Another profile is not penalised for the first one's traffic.
        assert!(limiter.check("b", 5));
    }

    #[test]
    fn limit_of_one_allows_exactly_one_request() {
        let limiter = RateLimiter::new();
        assert!(limiter.check("solo", 1));
        assert!(!limiter.check("solo", 1));
    }
}

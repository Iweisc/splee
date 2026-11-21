//! Per-organization limits on the public API and the MCP server, so one runaway script can't slow Splee down for everyone else.
//! Kept in memory because Splee runs as one process. A restart only hands every organization a fresh hour.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use crate::error::AppError;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub per_hour: u32,
    pub in_flight: u32,
    /// What the refusal counts, like "API requests".
    pub what: &'static str,
}

pub const LIMITS: Limits = Limits { per_hour: 10_000, in_flight: 150, what: "API requests" };
const WINDOW: Duration = Duration::from_secs(3600);

#[derive(Clone)]
pub struct RateLimiter {
    limits: Limits,
    orgs: Arc<Mutex<HashMap<Uuid, Usage>>>,
}

/// A fixed hour from an organization's first request in it. One entry per organization that ever used the API, which stays small.
struct Usage {
    window_start: Instant,
    used: u32,
    in_flight: u32,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(LIMITS)
    }
}

/// What every response says about the organization's hour, as RateLimit-Limit, RateLimit-Remaining and RateLimit-Reset.
#[derive(Debug, Clone, Copy)]
pub struct Quota {
    limit: u32,
    remaining: u32,
    reset_secs: u64,
}

/// One request in flight. Dropping it frees the slot, however the request ends.
pub struct Permit {
    limiter: RateLimiter,
    org_id: Uuid,
    pub quota: Quota,
}

#[derive(Debug)]
pub struct Refused {
    pub quota: Quota,
    pub retry_after_secs: u64,
    pub message: String,
}

impl RateLimiter {
    pub fn new(limits: Limits) -> Self {
        Self { limits, orgs: Arc::default() }
    }

    fn orgs(&self) -> MutexGuard<'_, HashMap<Uuid, Usage>> {
        self.orgs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn acquire(&self, org_id: Uuid) -> Result<Permit, Refused> {
        let now = Instant::now();
        let mut orgs = self.orgs();
        let usage = orgs.entry(org_id).or_insert(Usage { window_start: now, used: 0, in_flight: 0 });
        if now.duration_since(usage.window_start) >= WINDOW {
            usage.window_start = now;
            usage.used = 0;
        }
        let reset_secs = (WINDOW - now.duration_since(usage.window_start)).as_millis().div_ceil(1000) as u64;
        let quota = |used: u32| Quota { limit: self.limits.per_hour, remaining: self.limits.per_hour.saturating_sub(used), reset_secs };
        if usage.used >= self.limits.per_hour {
            let message = format!("This organization made {} {} this hour. Try again in {reset_secs} seconds", self.limits.per_hour, self.limits.what);
            return Err(Refused { quota: quota(usage.used), retry_after_secs: reset_secs, message });
        }
        if usage.in_flight >= self.limits.in_flight {
            let message = format!("This organization has {} {} running. Send the next when one finishes", self.limits.in_flight, self.limits.what);
            return Err(Refused { quota: quota(usage.used), retry_after_secs: 1, message });
        }
        usage.used += 1;
        usage.in_flight += 1;
        Ok(Permit { limiter: self.clone(), org_id, quota: quota(usage.used) })
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(usage) = self.limiter.orgs().get_mut(&self.org_id) {
            usage.in_flight = usage.in_flight.saturating_sub(1);
        }
    }
}

impl Quota {
    pub fn stamp(&self, headers: &mut HeaderMap) {
        headers.insert("ratelimit-limit", HeaderValue::from(self.limit));
        headers.insert("ratelimit-remaining", HeaderValue::from(self.remaining));
        headers.insert("ratelimit-reset", HeaderValue::from(self.reset_secs));
    }
}

impl Refused {
    pub fn stamp(&self, headers: &mut HeaderMap) {
        self.quota.stamp(headers);
        headers.insert(header::RETRY_AFTER, HeaderValue::from(self.retry_after_secs));
    }
}

/// The public API's 429. The MCP server answers with a JSON-RPC error instead and stamps the same headers.
impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        let mut res = AppError::TooManyRequests(self.message.clone()).into_response();
        self.stamp(res.headers_mut());
        res
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{Limits, RateLimiter};

    #[test]
    fn the_hour_and_the_requests_in_flight_are_capped_per_organization() {
        let limiter = RateLimiter::new(Limits { per_hour: 3, in_flight: 2, what: "API requests" });
        let (org, other) = (Uuid::new_v4(), Uuid::new_v4());

        let first = limiter.acquire(org).unwrap();
        let second = limiter.acquire(org).unwrap();
        assert_eq!((first.quota.remaining, second.quota.remaining), (2, 1));
        let Err(busy) = limiter.acquire(org) else { panic!("a third request in flight") };
        assert_eq!(busy.retry_after_secs, 1, "{busy:?}");
        assert_eq!(busy.quota.remaining, 1, "a refused request doesn't count");
        assert!(limiter.acquire(other).is_ok(), "another organization has its own slots");

        drop(first);
        let third = limiter.acquire(org).unwrap();
        assert_eq!(third.quota.remaining, 0);
        drop((second, third));
        let Err(spent) = limiter.acquire(org) else { panic!("a fourth request this hour") };
        assert!(spent.retry_after_secs > 3500 && spent.retry_after_secs <= 3600, "{spent:?}");
    }
}

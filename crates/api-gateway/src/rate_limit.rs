//! Per-user rate limiting (`docs/12-API-GATEWAY.md`).
//!
//! ## Why only `/agent/*`
//!
//! `docs/12`: "Per-user rate limits on `/agent/*` endpoints specifically (LLM
//! calls have real cost)." That is the whole reason. A candle read is a query
//! against a local database; an agent call is a paid round trip to a model that
//! can also be slow, so the two want different treatment and pretending
//! otherwise would mean rate-limiting the chart.
//!
//! ## A token bucket, because the alternative is worse
//!
//! A fixed window ("30 per minute") lets a client spend the whole minute's
//! budget in its first second and then wait -- which is a burst, not a limit. A
//! bucket refills continuously and caps the burst, so a person clicking
//! "Analyze" three times is fine and a loop is not.
//!
//! ## The clock is a parameter
//!
//! [`RateLimiter::check`] takes the time rather than reading it. A rate limiter
//! tested against `SystemTime` is either slow (sleeping) or flaky (racing), and
//! neither is worth it when passing a number costs nothing.
//!
//! ## Bounded memory
//!
//! One bucket per user, in a map that would grow forever on a public
//! deployment. Buckets that have refilled to full are indistinguishable from
//! absent ones, so [`RateLimiter::sweep`] drops them -- and the map is swept
//! whenever it grows past a threshold, which keeps the cost off the hot path
//! for the common case of one user.

use std::collections::HashMap;
use std::sync::Mutex;

use uuid::Uuid;

/// A bucket's shape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateLimit {
    /// Sustained requests per minute.
    pub per_minute: f64,
    /// How many may be spent at once.
    pub burst: f64,
}

impl Default for RateLimit {
    /// 30 a minute with a burst of 5.
    ///
    /// The sustained rate is generous for a person and mean for a loop. The
    /// burst is deliberately smaller than the minute's budget: three clicks in
    /// a row work, thirty do not.
    fn default() -> Self {
        Self {
            per_minute: 30.0,
            burst: 5.0,
        }
    }
}

impl RateLimit {
    /// Read the limit from the environment.
    ///
    /// A malformed value falls back to the default rather than refusing to
    /// start: a typo in an optional tuning variable should not take the service
    /// down, and the fallback is logged.
    #[must_use]
    pub fn from_env() -> Self {
        let default = Self::default();
        let per_minute = parse_env("AGENT_REQUESTS_PER_MINUTE", default.per_minute);
        let burst = parse_env("AGENT_BURST", default.burst);
        if per_minute <= 0.0 || burst <= 0.0 {
            tracing::warn!(
                per_minute,
                burst,
                "a rate limit must be positive; using the default"
            );
            return default;
        }
        Self { per_minute, burst }
    }

    /// Tokens added per second.
    #[must_use]
    fn refill_per_second(&self) -> f64 {
        self.per_minute / 60.0
    }
}

fn parse_env(name: &str, fallback: f64) -> f64 {
    match std::env::var(name) {
        Ok(raw) => raw.trim().parse().unwrap_or_else(|_| {
            tracing::warn!("{name}=`{raw}` is not a number; using {fallback}");
            fallback
        }),
        Err(_) => fallback,
    }
}

/// What one bucket holds.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    /// When it was last refilled, in seconds.
    last: f64,
}

/// Why a request was refused.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateLimited {
    /// Whole seconds until one token is available.
    pub retry_after_seconds: u64,
}

impl RateLimited {
    /// The `Retry-After` header value, as the HTTP spec wants it.
    #[must_use]
    pub fn retry_after_header(&self) -> String {
        // Zero is a legal value and means "immediately", which is what a
        // sub-second wait rounds to. Never negative.
        self.retry_after_seconds.to_string()
    }
}

/// A per-key token bucket.
#[derive(Debug)]
pub struct RateLimiter {
    limit: RateLimit,
    buckets: Mutex<HashMap<Uuid, Bucket>>,
}

/// Sweep once the map grows past this many keys.
const SWEEP_THRESHOLD: usize = 1024;

impl RateLimiter {
    /// Build a limiter.
    #[must_use]
    pub fn new(limit: RateLimit) -> Self {
        Self {
            limit,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// The limit in force, for reporting.
    #[must_use]
    pub const fn limit(&self) -> RateLimit {
        self.limit
    }

    /// Spend one token for `key`, or report how long to wait.
    ///
    /// # Errors
    /// Returns [`RateLimited`] when the bucket is empty.
    pub fn check(&self, key: Uuid, now_seconds: f64) -> Result<(), RateLimited> {
        let Ok(mut buckets) = self.buckets.lock() else {
            // A poisoned lock means a previous holder panicked mid-update. The
            // buckets are a rate limiter, not a ledger: allowing the request is
            // the safer failure, because refusing everything turns a panic into
            // an outage.
            tracing::error!("the rate limiter's lock was poisoned; allowing the request");
            return Ok(());
        };

        if buckets.len() >= SWEEP_THRESHOLD {
            self.sweep(&mut buckets, now_seconds);
        }

        let rate = self.limit.refill_per_second();
        let bucket = buckets.entry(key).or_insert(Bucket {
            tokens: self.limit.burst,
            last: now_seconds,
        });

        // Refill first, so a caller that waited is not refused on a stale count.
        let elapsed = (now_seconds - bucket.last).max(0.0);
        bucket.tokens = (bucket.tokens + elapsed * rate).min(self.limit.burst);
        bucket.last = now_seconds;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            return Ok(());
        }

        let deficit = 1.0 - bucket.tokens;
        let wait = if rate > 0.0 { deficit / rate } else { 60.0 };
        Err(RateLimited {
            // Round up: telling a client to retry in 0 seconds when the wait is
            // 0.4 is how you get a retry storm.
            retry_after_seconds: wait.ceil().max(0.0) as u64,
        })
    }

    /// How many buckets are tracked, for tests and reporting.
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.buckets.lock().map_or(0, |buckets| buckets.len())
    }

    /// Drop buckets that are indistinguishable from absent ones.
    ///
    /// A bucket at full is exactly what a caller that has never been seen would
    /// get, so removing it changes no behaviour and bounds the map.
    fn sweep(&self, buckets: &mut HashMap<Uuid, Bucket>, now_seconds: f64) {
        let rate = self.limit.refill_per_second();
        let burst = self.limit.burst;
        buckets.retain(|_, bucket| {
            let elapsed = (now_seconds - bucket.last).max(0.0);
            bucket.tokens + elapsed * rate < burst
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(per_minute: f64, burst: f64) -> RateLimiter {
        RateLimiter::new(RateLimit { per_minute, burst })
    }

    #[test]
    fn a_fresh_caller_may_spend_the_burst() {
        let limiter = limiter(60.0, 3.0);
        let user = Uuid::new_v4();
        for i in 0..3 {
            assert!(
                limiter.check(user, 0.0).is_ok(),
                "request {i} should be allowed"
            );
        }
        assert!(limiter.check(user, 0.0).is_err(), "the fourth must not be");
    }

    #[test]
    fn one_user_spending_their_budget_does_not_affect_another() {
        let limiter = limiter(60.0, 1.0);
        let noisy = Uuid::new_v4();
        let quiet = Uuid::new_v4();

        assert!(limiter.check(noisy, 0.0).is_ok());
        assert!(limiter.check(noisy, 0.0).is_err());
        // The whole point of a per-user limit.
        assert!(limiter.check(quiet, 0.0).is_ok());
    }

    #[test]
    fn the_bucket_refills_at_the_stated_rate() {
        // 60 a minute is one a second.
        let limiter = limiter(60.0, 1.0);
        let user = Uuid::new_v4();

        assert!(limiter.check(user, 0.0).is_ok());
        assert!(limiter.check(user, 0.0).is_err(), "empty at t=0");
        assert!(limiter.check(user, 0.5).is_err(), "still empty at t=0.5");
        assert!(limiter.check(user, 1.0).is_ok(), "one token by t=1");
    }

    #[test]
    fn the_burst_is_a_ceiling_not_a_credit() {
        // Idle for an hour; the bucket must not have grown past its size.
        let limiter = limiter(60.0, 2.0);
        let user = Uuid::new_v4();
        assert!(limiter.check(user, 0.0).is_ok());
        assert!(limiter.check(user, 3_600.0).is_ok());
        assert!(limiter.check(user, 3_600.0).is_ok());
        assert!(
            limiter.check(user, 3_600.0).is_err(),
            "an hour of idling must not buy more than the burst"
        );
    }

    #[test]
    fn a_refusal_says_how_long_to_wait_and_rounds_up() {
        // 6 a minute is one every ten seconds.
        let limiter = limiter(6.0, 1.0);
        let user = Uuid::new_v4();
        assert!(limiter.check(user, 0.0).is_ok());

        let refusal = limiter.check(user, 0.0).expect_err("empty");
        assert_eq!(refusal.retry_after_seconds, 10);
        assert_eq!(refusal.retry_after_header(), "10");

        // 0.1s in, 9.9s remain -> 10, not 9: rounding down would invite a
        // retry that is still refused.
        let refusal = limiter.check(user, 0.1).expect_err("still empty");
        assert_eq!(refusal.retry_after_seconds, 10);
    }

    #[test]
    fn a_clock_that_goes_backwards_does_not_mint_tokens() {
        // NTP can step the clock. `elapsed.max(0.0)` is what stops a step
        // backwards from being a refund.
        let limiter = limiter(60.0, 1.0);
        let user = Uuid::new_v4();
        assert!(limiter.check(user, 100.0).is_ok());
        assert!(
            limiter.check(user, 50.0).is_err(),
            "no refund for going back"
        );
        assert!(
            limiter.check(user, 101.0).is_ok(),
            "and time still moves on"
        );
    }

    #[test]
    fn idle_buckets_are_swept_away() {
        let limiter = limiter(60.0, 5.0);
        for _ in 0..10 {
            limiter.check(Uuid::new_v4(), 0.0).ok();
        }
        assert_eq!(limiter.tracked(), 10);

        // A minute later every bucket is full again, so none of them is
        // carrying information.
        limiter.sweep(&mut limiter.buckets.lock().unwrap(), 60.0);
        assert_eq!(limiter.tracked(), 0);
    }

    #[test]
    fn a_bucket_that_is_still_spent_is_not_swept() {
        let limiter = limiter(6.0, 5.0); // one token every 10s
        let user = Uuid::new_v4();
        for _ in 0..5 {
            limiter.check(user, 0.0).ok();
        }
        // One second later it holds half a token, which is not "full".
        limiter.sweep(&mut limiter.buckets.lock().unwrap(), 1.0);
        assert_eq!(limiter.tracked(), 1, "a spent bucket must survive");
    }

    #[test]
    fn the_default_is_generous_for_a_person_and_mean_for_a_loop() {
        let limit = RateLimit::default();
        assert_eq!(limit.per_minute, 30.0);
        // Fewer at once than the minute's budget: three clicks work, thirty do
        // not.
        assert!(limit.burst < limit.per_minute);
    }
}

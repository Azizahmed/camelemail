//! In-memory rate limits (token bucket per key) and the progressive sign-in
//! lockout (design §12: 5 failures → 1 min, doubling, at most 1 h).
//!
//! Buckets are per process. Lockout state for real users lives in Postgres
//! (`users.failed_logins`, `locked_until`) so it holds across processes; the
//! in-memory [`Lockouts`] only covers e-mails that have no account, so those
//! answer exactly like real ones.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Failures allowed before the first lockout.
pub const FREE_FAILURES: i32 = 5;

/// Lockout after `failures` consecutive failures, or `None` while under the
/// threshold: `base` at the 5th failure, doubling each time, capped at `max`.
pub fn lockout_for(failures: i32, base: Duration, max: Duration) -> Option<Duration> {
    if failures < FREE_FAILURES {
        return None;
    }
    let doublings = (failures - FREE_FAILURES).min(20) as u32;
    Some(base.saturating_mul(1u32 << doublings).min(max))
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

/// Token bucket per key (an IP, an e-mail).
pub struct Limiter {
    burst: f64,
    per_sec: f64,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl Limiter {
    pub fn new(burst: u32, per_sec: f64) -> Self {
        Limiter { burst: burst.max(1) as f64, per_sec: per_sec.max(0.0001), buckets: Mutex::new(HashMap::new()) }
    }

    /// Takes one token, or returns the seconds until one is available.
    pub fn check(&self, key: &str) -> Result<(), u64> {
        let now = Instant::now();
        let mut map = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        if map.len() > 50_000 {
            // Forget keys whose bucket has refilled; they behave as new.
            let (burst, rate) = (self.burst, self.per_sec);
            map.retain(|_, b| b.tokens + now.duration_since(b.at).as_secs_f64() * rate < burst);
        }
        let b = map.entry(key.to_string()).or_insert(Bucket { tokens: self.burst, at: now });
        b.tokens = (b.tokens + now.duration_since(b.at).as_secs_f64() * self.per_sec).min(self.burst);
        b.at = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else {
            Err(((1.0 - b.tokens) / self.per_sec).ceil() as u64)
        }
    }
}

/// Failed-login counters for e-mails without an account.
pub struct Lockouts {
    base: Duration,
    max: Duration,
    map: Mutex<HashMap<String, (i32, Option<Instant>)>>,
}

impl Lockouts {
    pub fn new(base: Duration, max: Duration) -> Self {
        Lockouts { base, max, map: Mutex::new(HashMap::new()) }
    }

    /// Seconds left if `key` is locked.
    pub fn locked(&self, key: &str) -> Option<u64> {
        let map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        let (_, until) = map.get(key)?;
        let left = until.as_ref()?.checked_duration_since(Instant::now())?;
        Some(left.as_secs().max(1))
    }

    /// Records a failure; returns the new lockout, if one starts.
    pub fn fail(&self, key: &str) -> Option<Duration> {
        let mut map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        if map.len() > 50_000 {
            let now = Instant::now();
            map.retain(|_, (_, until)| until.is_some_and(|u| u > now));
        }
        let e = map.entry(key.to_string()).or_insert((0, None));
        e.0 += 1;
        let lock = lockout_for(e.0, self.base, self.max);
        e.1 = lock.map(|d| Instant::now() + d);
        lock
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockout_schedule() {
        let (b, m) = (Duration::from_secs(60), Duration::from_secs(3600));
        assert_eq!(lockout_for(4, b, m), None);
        assert_eq!(lockout_for(5, b, m), Some(Duration::from_secs(60)));
        assert_eq!(lockout_for(6, b, m), Some(Duration::from_secs(120)));
        assert_eq!(lockout_for(10, b, m), Some(Duration::from_secs(1920)));
        assert_eq!(lockout_for(11, b, m), Some(Duration::from_secs(3600)));
        assert_eq!(lockout_for(500, b, m), Some(Duration::from_secs(3600)));
    }

    #[test]
    fn bucket_drains_and_refills() {
        let l = Limiter::new(3, 1000.0);
        assert!(l.check("a").is_ok());
        assert!(l.check("a").is_ok());
        assert!(l.check("a").is_ok());
        // A different key has its own bucket.
        assert!(l.check("b").is_ok());
        std::thread::sleep(Duration::from_millis(5));
        assert!(l.check("a").is_ok());
        let slow = Limiter::new(1, 0.01);
        assert!(slow.check("x").is_ok());
        assert!(slow.check("x").unwrap_err() >= 99);
    }

    #[test]
    fn unknown_emails_lock_like_real_ones() {
        let l = Lockouts::new(Duration::from_secs(60), Duration::from_secs(3600));
        for _ in 0..4 {
            assert!(l.fail("nobody@x").is_none());
        }
        assert!(l.locked("nobody@x").is_none());
        assert_eq!(l.fail("nobody@x"), Some(Duration::from_secs(60)));
        assert!(l.locked("nobody@x").unwrap() > 50);
    }
}

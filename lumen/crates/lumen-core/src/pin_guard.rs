//! Throttles wrong PIN guesses. A profile PIN is a 4+ character secret, so without a limit a
//! caller with access to the commands could try every short PIN in minutes. After a few free
//! attempts each further failure locks that profile out for twice as long as the last, up to a
//! cap. State is in memory only: restarting the app resets it, which is acceptable because the
//! PIN gates the UI of a running session, not the data at rest.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Wrong guesses allowed before the first lockout.
const FREE_ATTEMPTS: u32 = 5;
const BASE_LOCKOUT: Duration = Duration::from_secs(30);
const MAX_LOCKOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
struct Entry {
    failures: u32,
    locked_until: Option<Instant>,
}

#[derive(Default)]
pub struct AttemptLimiter {
    entries: HashMap<i64, Entry>,
}

impl AttemptLimiter {
    /// `Err(remaining)` while the profile is locked out.
    pub fn check(&self, profile_id: i64, now: Instant) -> Result<(), Duration> {
        match self.entries.get(&profile_id).and_then(|entry| entry.locked_until) {
            Some(until) if until > now => Err(until - now),
            _ => Ok(()),
        }
    }

    pub fn record_failure(&mut self, profile_id: i64, now: Instant) {
        let entry = self.entries.entry(profile_id).or_default();
        entry.failures = entry.failures.saturating_add(1);
        if entry.failures >= FREE_ATTEMPTS {
            let doublings = (entry.failures - FREE_ATTEMPTS).min(16);
            let lockout = BASE_LOCKOUT
                .checked_mul(1u32 << doublings)
                .unwrap_or(MAX_LOCKOUT)
                .min(MAX_LOCKOUT);
            entry.locked_until = Some(now + lockout);
        }
    }

    pub fn record_success(&mut self, profile_id: i64) {
        self.entries.remove(&profile_id);
    }
}

static LIMITER: LazyLock<Mutex<AttemptLimiter>> = LazyLock::new(Default::default);

/// Seconds a caller must wait before the next guess, or `None` when a guess is allowed.
pub fn locked_out_for(profile_id: i64) -> Option<Duration> {
    let limiter = LIMITER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    limiter.check(profile_id, Instant::now()).err()
}

pub fn record_failure(profile_id: i64) {
    let mut limiter = LIMITER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    limiter.record_failure(profile_id, Instant::now());
}

pub fn record_success(profile_id: i64) {
    let mut limiter = LIMITER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    limiter.record_success(profile_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_guesses_are_free_then_the_profile_locks() {
        let mut limiter = AttemptLimiter::default();
        let now = Instant::now();
        for _ in 0..FREE_ATTEMPTS - 1 {
            limiter.record_failure(1, now);
            assert!(limiter.check(1, now).is_ok(), "still within the free attempts");
        }
        limiter.record_failure(1, now);
        assert_eq!(limiter.check(1, now), Err(BASE_LOCKOUT));
    }

    #[test]
    fn lockout_expires_and_doubles_on_repeat_failures() {
        let mut limiter = AttemptLimiter::default();
        let start = Instant::now();
        for _ in 0..FREE_ATTEMPTS {
            limiter.record_failure(1, start);
        }
        let after = start + BASE_LOCKOUT + Duration::from_secs(1);
        assert!(limiter.check(1, after).is_ok(), "lockout ends on its own");

        limiter.record_failure(1, after);
        assert_eq!(limiter.check(1, after), Err(BASE_LOCKOUT * 2));
    }

    #[test]
    fn lockout_is_capped() {
        let mut limiter = AttemptLimiter::default();
        let now = Instant::now();
        for _ in 0..200 {
            limiter.record_failure(1, now);
        }
        assert_eq!(limiter.check(1, now), Err(MAX_LOCKOUT));
    }

    #[test]
    fn success_clears_the_count_and_profiles_are_independent() {
        let mut limiter = AttemptLimiter::default();
        let now = Instant::now();
        for _ in 0..FREE_ATTEMPTS {
            limiter.record_failure(1, now);
        }
        assert!(limiter.check(2, now).is_ok(), "another profile is unaffected");
        limiter.record_success(1);
        assert!(limiter.check(1, now).is_ok());
        limiter.record_failure(1, now);
        assert!(limiter.check(1, now).is_ok(), "the count restarted from zero");
    }
}

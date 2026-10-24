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
        match self
            .entries
            .get(&profile_id)
            .and_then(|entry| entry.locked_until)
        {
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

    /// Checks the lockout and, if the caller may proceed, immediately charges this call as a
    /// failure before it does anything async. Without this, a caller checking the lockout, then
    /// awaiting the (comparatively slow) PIN verification, then recording the result, leaves a
    /// window open the whole time the check and the record are two separate steps: a burst of
    /// concurrent guesses fired at once can all pass the check before any of them records a
    /// failure, trying far more PINs than `FREE_ATTEMPTS` before the limiter notices any of
    /// them. Charging the attempt up front, inside the one lock acquisition that also does the
    /// check, bounds a concurrent burst the same way a sequential one already is. A correct
    /// guess un-charges it via `record_success`, so getting it right on, say, the third try
    /// still only ever costs what three sequential guesses would have.
    pub fn begin_attempt(&mut self, profile_id: i64, now: Instant) -> Result<(), Duration> {
        self.check(profile_id, now)?;
        self.record_failure(profile_id, now);
        Ok(())
    }
}

static LIMITER: LazyLock<Mutex<AttemptLimiter>> = LazyLock::new(Default::default);

/// Seconds a caller must wait before the next guess, or `None` when a guess is allowed.
pub fn locked_out_for(profile_id: i64) -> Option<Duration> {
    let limiter = LIMITER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    limiter.check(profile_id, Instant::now()).err()
}

pub fn record_failure(profile_id: i64) {
    let mut limiter = LIMITER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    limiter.record_failure(profile_id, Instant::now());
}

pub fn record_success(profile_id: i64) {
    let mut limiter = LIMITER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    limiter.record_success(profile_id);
}

/// `Err(remaining)` while locked out; otherwise this guess is already charged against the limit
/// (see `AttemptLimiter::begin_attempt`) before the caller does anything async.
pub fn begin_attempt(profile_id: i64) -> Result<(), Duration> {
    let mut limiter = LIMITER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    limiter.begin_attempt(profile_id, Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Proves the fix for the parallel-guess race: a check-then-record pattern across an
    /// await point (as `authorize_profile` used to use) lets any number of concurrent guesses
    /// through before the first one's result is recorded, since none of them observes any of
    /// the others' failures until after the fact. `begin_attempt` charges the attempt in the
    /// same lock acquisition as the check, so a burst can never get more tries than a
    /// sequential run of guesses would.
    #[test]
    fn concurrent_guesses_cannot_outrun_the_limit() {
        use std::sync::Arc;
        use std::thread;

        let limiter = Arc::new(Mutex::new(AttemptLimiter::default()));
        let now = Instant::now();
        let handles: Vec<_> = (0..50)
            .map(|_| {
                let limiter = limiter.clone();
                thread::spawn(move || limiter.lock().unwrap().begin_attempt(1, now).is_ok())
            })
            .collect();
        let allowed = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|ok| *ok)
            .count();
        assert_eq!(
            allowed as u32, FREE_ATTEMPTS,
            "a burst of concurrent guesses must not get more tries than a sequential one would"
        );
    }

    #[test]
    fn first_guesses_are_free_then_the_profile_locks() {
        let mut limiter = AttemptLimiter::default();
        let now = Instant::now();
        for _ in 0..FREE_ATTEMPTS - 1 {
            limiter.record_failure(1, now);
            assert!(
                limiter.check(1, now).is_ok(),
                "still within the free attempts"
            );
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
        assert!(
            limiter.check(2, now).is_ok(),
            "another profile is unaffected"
        );
        limiter.record_success(1);
        assert!(limiter.check(1, now).is_ok());
        limiter.record_failure(1, now);
        assert!(
            limiter.check(1, now).is_ok(),
            "the count restarted from zero"
        );
    }
}

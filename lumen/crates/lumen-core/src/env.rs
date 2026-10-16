//! Environment abstraction (`Env`) for HTTP requests, wall clock, and application storage paths.
//!
//! `lumen-core` uses this trait so unit and integration tests can run with deterministic clocks,
//! temporary SQLite directories, and custom HTTP clients without initializing GTK or WebKit.

use crate::error::{PlayerError, Result};
use crate::netguard;
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub trait Env: Send + Sync + 'static {
    /// Current wall-clock timestamp in Unix seconds.
    fn now_secs(&self) -> i64 {
        Utc::now().timestamp()
    }

    /// Current wall-clock timestamp in Unix milliseconds.
    fn now_millis(&self) -> i64 {
        Utc::now().timestamp_millis()
    }

    /// Application data directory used for SQLite, thumbnail cache, and managed binaries.
    fn app_data_dir(&self) -> Option<&Path>;

    /// Builds an HTTP client guarded by `netguard` (public-only unless `allow_lan` is true).
    fn guarded_http_client(&self, allow_lan: bool, timeout: Duration) -> Result<reqwest::Client> {
        netguard::guarded_client(allow_lan)
            .timeout(timeout)
            .user_agent("Lumen/0.1")
            .build()
            .map_err(PlayerError::from)
    }

    /// Builds an unguarded HTTP client for trusted first-party endpoints (e.g. account sync).
    fn http_client(&self, timeout: Duration) -> Result<reqwest::Client> {
        reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(PlayerError::from)
    }
}

/// Default production/desktop environment backed by OS clock, `reqwest`, and a fixed data dir.
#[derive(Debug, Clone, Default)]
pub struct SystemEnv {
    pub data_dir: Option<PathBuf>,
}

impl SystemEnv {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        Self { data_dir }
    }
}

impl Env for SystemEnv {
    fn app_data_dir(&self) -> Option<&Path> {
        self.data_dir.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedClockEnv {
        fixed_ms: i64,
        dir: PathBuf,
    }

    impl Env for FixedClockEnv {
        fn now_secs(&self) -> i64 {
            self.fixed_ms / 1000
        }

        fn now_millis(&self) -> i64 {
            self.fixed_ms
        }

        fn app_data_dir(&self) -> Option<&Path> {
            Some(&self.dir)
        }
    }

    #[test]
    fn custom_env_supplies_deterministic_clock_and_storage_dir() {
        let env = FixedClockEnv {
            fixed_ms: 1_700_000_000_500,
            dir: PathBuf::from("/tmp/lumen-core-test"),
        };
        assert_eq!(env.now_secs(), 1_700_000_000);
        assert_eq!(env.now_millis(), 1_700_000_000_500);
        assert_eq!(env.app_data_dir(), Some(Path::new("/tmp/lumen-core-test")));
    }
}

//! Search-provider API tokens and the TMDB API key are kept in the OS keychain.
//! The SQLite settings table only ever stores provider configuration with the
//! `token` field removed.

use crate::error::{PlayerError, Result};
use serde_json::Value;

const KEYCHAIN_SERVICE: &str = "dev.lumen.app.search-providers";
const TMDB_KEYCHAIN_SERVICE: &str = "dev.lumen.app.tmdb";
const TMDB_KEYCHAIN_ACCOUNT: &str = "api-key";

fn keychain_error(error: keyring::Error) -> PlayerError {
    PlayerError::Io(format!("OS keychain unavailable: {error}"))
}

fn entry(service: &str, provider_id: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(service, provider_id).map_err(keychain_error)
}

/// Returns the provider's id, which keys its keychain entry. Ids must be non-empty strings.
pub fn provider_id(provider: &Value) -> Result<String> {
    match provider.get("id").and_then(Value::as_str).map(str::trim) {
        Some(id) if !id.is_empty() => Ok(id.to_string()),
        _ => Err(PlayerError::InvalidArg("Each search provider needs a non-empty id".into())),
    }
}

/// Removes the `token` field from a provider and returns it so the caller can
/// decide where it goes. A missing field returns `None`; a non-string value is rejected.
pub fn take_token(provider: &mut Value) -> Result<Option<String>> {
    let Some(object) = provider.as_object_mut() else {
        return Err(PlayerError::InvalidArg("Each search provider must be an object".into()));
    };
    match object.remove("token") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(token)) => Ok(Some(token)),
        Some(_) => Err(PlayerError::InvalidArg("A provider token must be a string".into())),
    }
}

/// Stores a token in the keychain. Errors propagate, so tokens are never saved in plaintext as a fallback.
pub fn store_token(provider_id: &str, token: &str) -> Result<()> {
    store_token_in(KEYCHAIN_SERVICE, provider_id, token)
}

fn store_token_in(service: &str, provider_id: &str, token: &str) -> Result<()> {
    entry(service, provider_id)?.set_password(token).map_err(keychain_error)
}

pub fn load_token(provider_id: &str) -> Result<Option<String>> {
    load_token_in(KEYCHAIN_SERVICE, provider_id)
}

fn load_token_in(service: &str, provider_id: &str) -> Result<Option<String>> {
    match entry(service, provider_id)?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(keychain_error(error)),
    }
}

pub fn delete_token(provider_id: &str) -> Result<()> {
    delete_token_in(KEYCHAIN_SERVICE, provider_id)
}

fn delete_token_in(service: &str, provider_id: &str) -> Result<()> {
    match entry(service, provider_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(keychain_error(error)),
    }
}

fn tmdb_entry() -> Result<keyring::Entry> {
    keyring::Entry::new(TMDB_KEYCHAIN_SERVICE, TMDB_KEYCHAIN_ACCOUNT).map_err(keychain_error)
}

/// Stores the TMDB API key in the keychain. Errors propagate rather than falling back to plaintext.
pub fn store_tmdb_key(key: &str) -> Result<()> {
    tmdb_entry()?.set_password(key).map_err(keychain_error)
}

pub fn load_tmdb_key() -> Result<Option<String>> {
    match tmdb_entry()?.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(keychain_error(error)),
    }
}

pub fn delete_tmdb_key() -> Result<()> {
    match tmdb_entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(keychain_error(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn take_token_removes_the_field_and_returns_it() {
        let mut provider = json!({"id": "catalog", "token": "secret"});
        assert_eq!(take_token(&mut provider).unwrap().as_deref(), Some("secret"));
        assert!(provider.get("token").is_none());
    }

    #[test]
    fn take_token_distinguishes_absent_from_empty() {
        let mut absent = json!({"id": "catalog"});
        assert_eq!(take_token(&mut absent).unwrap(), None);
        let mut empty = json!({"id": "catalog", "token": ""});
        assert_eq!(take_token(&mut empty).unwrap().as_deref(), Some(""));
    }

    #[test]
    fn take_token_rejects_non_string_values() {
        let mut provider = json!({"id": "catalog", "token": 42});
        assert!(take_token(&mut provider).is_err());
    }

    #[test]
    fn provider_id_requires_a_non_empty_string() {
        assert_eq!(provider_id(&json!({"id": " catalog "})).unwrap(), "catalog");
        assert!(provider_id(&json!({"id": "  "})).is_err());
        assert!(provider_id(&json!({"name": "no id"})).is_err());
    }

    /// Keychain service used only by tests, so they never touch real provider tokens.
    const TEST_KEYCHAIN_SERVICE: &str = "dev.lumen.app.search-providers.test";

    /// Removes the scratch keychain entry when the test ends, even if an assertion fails.
    struct ScratchToken(String);

    impl Drop for ScratchToken {
        fn drop(&mut self) {
            let _ = delete_token_in(TEST_KEYCHAIN_SERVICE, &self.0);
        }
    }

    /// Writes, reads, and deletes a token in the real OS keychain under a throwaway id and a
    /// test-only service name. If no keychain is reachable (for example a headless Linux session
    /// with no Secret Service) the probe write fails and the test skips. Setting
    /// `LUMEN_REQUIRE_KEYCHAIN=1` turns that skip into a failure, so a machine that is meant to
    /// have a keychain cannot pass silently.
    #[test]
    fn provider_token_round_trips_through_the_os_keychain() {
        let id = format!("lumen-test-{}", uuid::Uuid::new_v4());
        if let Err(error) = store_token_in(TEST_KEYCHAIN_SERVICE, &id, "probe") {
            if std::env::var("LUMEN_REQUIRE_KEYCHAIN").as_deref() == Ok("1") {
                panic!("LUMEN_REQUIRE_KEYCHAIN=1 but the OS keychain is unavailable: {error}");
            }
            eprintln!("skipping keychain round trip: no OS keychain available ({error})");
            return;
        }
        let _cleanup = ScratchToken(id.clone());

        store_token_in(TEST_KEYCHAIN_SERVICE, &id, "round-trip-token").expect("keychain write");
        assert_eq!(
            load_token_in(TEST_KEYCHAIN_SERVICE, &id).expect("keychain read").as_deref(),
            Some("round-trip-token")
        );

        delete_token_in(TEST_KEYCHAIN_SERVICE, &id).expect("keychain delete");
        assert_eq!(
            load_token_in(TEST_KEYCHAIN_SERVICE, &id).expect("keychain read after delete"),
            None
        );
        delete_token_in(TEST_KEYCHAIN_SERVICE, &id).expect("deleting a missing entry is not an error");
    }
}

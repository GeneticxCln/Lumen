use crate::error::{PlayerError, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

/// Minimum and maximum PIN length, counted in characters.
const MIN_PIN_CHARS: usize = 4;
const MAX_PIN_CHARS: usize = 64;

/// A profile PIN gates that profile's UI on this device. It is deliberately **not** a claim
/// about the data: the library, the database and the media files are not encrypted, and
/// anyone with access to the machine can read them. It stops someone walking up to the app
/// and browsing another profile's history or saved titles.
pub fn validate_pin(pin: &str) -> Result<()> {
    let length = pin.chars().count();
    if length < MIN_PIN_CHARS {
        return Err(PlayerError::InvalidArg(format!(
            "A PIN needs at least {} characters",
            MIN_PIN_CHARS
        )));
    }
    if length > MAX_PIN_CHARS {
        return Err(PlayerError::InvalidArg(format!(
            "A PIN is limited to {} characters",
            MAX_PIN_CHARS
        )));
    }
    if pin.chars().any(char::is_control) {
        return Err(PlayerError::InvalidArg(
            "A PIN cannot contain control characters".into(),
        ));
    }
    Ok(())
}

/// Hash a PIN with Argon2id and a fresh random salt. Only the hash is stored.
pub fn hash_pin(pin: &str) -> Result<String> {
    validate_pin(pin)?;

    // A fresh random salt per hash. UUID v4 bytes come from the OS CSPRNG, which is what
    // `SaltString::generate` would draw from anyway; a salt only has to be unique, not secret.
    let salt = SaltString::encode_b64(uuid::Uuid::new_v4().as_bytes())
        .map_err(|e| PlayerError::InvalidArg(format!("Could not salt the PIN: {}", e)))?;
    Argon2::default()
        .hash_password(pin.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| PlayerError::InvalidArg(format!("Could not secure the PIN: {}", e)))
}

/// Constant-time verification of a PIN against a stored hash. A malformed or missing hash
/// verifies nothing.
pub fn verify_pin(pin: &str, stored_hash: &str) -> bool {
    match PasswordHash::new(stored_hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(pin.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pin_hash_round_trip_and_rejection() {
        let hash = hash_pin("4821").expect("hashing should succeed");

        assert!(verify_pin("4821", &hash));
        assert!(!verify_pin("4822", &hash));
        assert!(!verify_pin("", &hash));
        assert!(hash.starts_with("$argon2id$"), "stored value is an Argon2id hash");
        assert!(!hash.contains("4821"), "the PIN itself is never stored");
    }

    #[test]
    fn test_the_same_pin_hashes_differently_each_time() {
        let first = hash_pin("4821").unwrap();
        let second = hash_pin("4821").unwrap();

        assert_ne!(first, second, "each hash gets its own random salt");
        assert!(verify_pin("4821", &first) && verify_pin("4821", &second));
    }

    #[test]
    fn test_pin_validation_bounds() {
        assert!(validate_pin("1234").is_ok());
        assert!(validate_pin("correct horse battery staple").is_ok());

        assert!(validate_pin("123").is_err(), "too short");
        assert!(validate_pin(&"9".repeat(MAX_PIN_CHARS + 1)).is_err(), "too long");
        assert!(validate_pin("12\n34").is_err(), "control characters rejected");
        assert!(hash_pin("123").is_err(), "hashing validates too");
    }

    #[test]
    fn test_corrupt_hash_never_verifies() {
        assert!(!verify_pin("4821", "not-a-hash"));
        assert!(!verify_pin("4821", ""));
    }
}

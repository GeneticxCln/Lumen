//! Verifies that a release's `SHA256SUMS` file was signed by the key that ships with Lumen,
//! layered on top of the checksum check `update.rs` already does. The checksum alone proves a
//! download was not corrupted in transit; it does not prove who published it, since anyone who
//! can publish a release to the repository can publish an update and its own checksums. A
//! signature over the checksums file closes that gap: it can only be produced by whoever holds
//! the matching secret key, which never leaves the maintainer's machine and is not in this repo.

use minisign_verify::{PublicKey, Signature};

use crate::error::{PlayerError, Result};

/// Generated once offline; the matching secret key never touches this repository. Committed in
/// cleartext by design — a minisign public key is meant to be public, the same way a TLS
/// certificate's public key is.
#[cfg(not(test))]
const RELEASE_PUBLIC_KEY: &str = "RWQUSCl7vuuqLvO33QtXZ1NoLOVj3EUJJvLO9xqFmLHx6MeKAPbAEbo+";
/// A throwaway key whose secret half exists only on the machine that produced the test fixtures
/// below (here and in `update.rs`) — swapped in under `cfg(test)` so those fixtures can carry a
/// genuinely valid signature without the real release secret key ever needing to exist outside
/// the maintainer's own machine, in this repo or anywhere test-reachable.
#[cfg(test)]
const RELEASE_PUBLIC_KEY: &str = "RWSXv6TLERTKxN/z8wy/gQEbvMSMmQ/k5Fx6qj8Yui2SOgi2ypVQaqQT";

/// Checks that `signature_text` (the contents of a `SHA256SUMS.minisig` release asset) is a
/// valid signature, by the release key above, over `checksums_bytes` (the `SHA256SUMS` the
/// signature was published alongside). Rejects a signature from any other key, including an
/// otherwise well-formed one, and rejects legacy (non-prehashed) signatures, which this key has
/// never produced.
pub fn verify_checksums_signature(checksums_bytes: &[u8], signature_text: &str) -> Result<()> {
    let public_key = PublicKey::from_base64(RELEASE_PUBLIC_KEY)
        .map_err(|e| PlayerError::Parse(format!("the baked-in release key is invalid: {e}")))?;
    let signature = Signature::decode(signature_text)
        .map_err(|_| PlayerError::Parse("SHA256SUMS.minisig is not a valid signature".into()))?;
    public_key
        .verify(checksums_bytes, &signature, false)
        .map_err(|_| {
            PlayerError::Parse(
                "SHA256SUMS is not signed by the Lumen release key; refusing to trust it".into(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Produced by the secret half of the cfg(test) key above (a throwaway keypair; its secret
    // half is not kept anywhere, including this repository, beyond having signed this one fixture).
    const VALID_SIGNATURE: &str = "untrusted comment: signature from minisign secret key
RUSXv6TLERTKxA5IxEIseGo7NvOvueoje1nFgSDcwxpDMDbtlSRkqiguvm4yqDHl9GstZ1krsJW5i06PqTE5QC5w2U2FzXRMhg0=
trusted comment: timestamp:1792861535\tfile:clean.txt\thashed
QJYR2wbzaJZbwoRAkBwp8eIhghJouaXao6jbG5M/xtptnsJ1S87LxLjocMgtpC/0wF6UB30dZ+0pFiaU1CeKCQ==";
    const SIGNED_CONTENT: &[u8] = b"hello world";

    #[test]
    fn a_valid_signature_from_the_right_key_verifies() {
        verify_checksums_signature(SIGNED_CONTENT, VALID_SIGNATURE).unwrap();
    }

    #[test]
    fn a_tampered_file_fails_verification() {
        assert!(verify_checksums_signature(b"hello world!", VALID_SIGNATURE).is_err());
    }

    #[test]
    fn a_malformed_signature_is_rejected_not_panicked_on() {
        assert!(verify_checksums_signature(SIGNED_CONTENT, "not a signature").is_err());
        assert!(verify_checksums_signature(SIGNED_CONTENT, "").is_err());
    }

    #[test]
    fn a_signature_from_a_different_key_is_rejected() {
        // A signature produced by yet another throwaway key over the same content; the baked-in
        // key above never produced it, so it must not verify.
        let other_keys_signature = "untrusted comment: signature from minisign secret key
RUSTqY2Em/Gz8TIgrQLfH1DkycBxK0ARB6FndcKcSDWKiiIGAR1q88aLrl1MJORiyQgTAjrlURustI2jdUmBKEtcpPJUGBoymwQ=
trusted comment: timestamp:1792861870\tfile:clean.txt\thashed
mvDgPQ18vnhkjjaBPCaFJ2opNsKKCFQKlXHUDwtiX+DuGEBc/gj/OYPQCwvFNKJRO5+wjlwoGbFk+cfP2a5xCg==";
        assert!(verify_checksums_signature(SIGNED_CONTENT, other_keys_signature).is_err());
    }

    #[test]
    fn the_baked_in_release_key_itself_decodes() {
        PublicKey::from_base64(RELEASE_PUBLIC_KEY)
            .expect("the committed release public key must always be well-formed");
    }
}

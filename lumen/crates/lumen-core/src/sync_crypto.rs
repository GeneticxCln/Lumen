//! End-to-end encryption for sync.
//!
//! A sync group is one random 32-byte `root`. Everything else is derived from it with HKDF, so a
//! second device needs only the root (typed in as the "sync key"). The server learns an opaque
//! group id, opaque item ids, timestamps and sizes, and never a URL, a title or a position.
//!
//! Four keys come from the root, each with its own HKDF label so that knowing one reveals
//! nothing about another. In particular the server is shown the *authentication* key (to prove
//! the caller belongs to the group) but never the *encryption* key.
//!
//! Every record is sealed with XChaCha20-Poly1305. The authenticated data binds the ciphertext to
//! its group, item id, timestamp and deleted flag, so the server cannot move a blob to another
//! item or change when it was written without the open step failing. It can still drop records
//! or serve an older valid version of one; last-write-wins on the client limits what that does.

use crate::error::{PlayerError, Result};
use crate::sync::SyncItem;
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use data_encoding::{Encoding, Specification};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

const NONCE_LEN: usize = 24;
const ROOT_LEN: usize = 32;
const KEY_ENCODED_LEN: usize = 52; // 32 bytes of Crockford base32
const CHECK_LEN: usize = 4;
const CROCKFORD: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Leading zero bits required of the proof of work that registers a new group.
pub const POW_BITS: u32 = 20;

static BASE32: LazyLock<Encoding> = LazyLock::new(|| {
    let mut spec = Specification::new();
    spec.symbols.push_str(CROCKFORD);
    spec.encoding()
        .expect("the Crockford alphabet is a valid base32 specification")
});

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// A record as the server stores it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SealedItem {
    pub id: String,
    pub updated_at: i64,
    pub deleted: bool,
    pub blob: String,
    /// The server's write-order position for this record (see the Worker's `sync_seq`). Absent
    /// on a push (the client never assigns this) and on a legacy `since`-based pull; present on
    /// a `since_seq`-based pull, which is what the client now always uses to page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
}

/// What is encrypted: the real kind and key, which the server must not see.
#[derive(Serialize, Deserialize)]
struct Plain {
    kind: String,
    key: String,
    payload: Value,
}

/// The keys of one sync group.
pub struct SyncKeys {
    sync_id: [u8; 16],
    auth_key: [u8; 32],
    enc_key: [u8; 32],
    index_key: [u8; 32],
}

impl SyncKeys {
    pub fn from_root(root: &[u8; ROOT_LEN]) -> Self {
        let hkdf = Hkdf::<Sha256>::new(None, root);
        let mut keys = Self {
            sync_id: [0; 16],
            auth_key: [0; 32],
            enc_key: [0; 32],
            index_key: [0; 32],
        };
        hkdf.expand(b"lumen/sync-id/v1", &mut keys.sync_id)
            .expect("16 bytes is a valid HKDF length");
        hkdf.expand(b"lumen/auth/v1", &mut keys.auth_key)
            .expect("32 bytes is a valid HKDF length");
        hkdf.expand(b"lumen/enc/v1", &mut keys.enc_key)
            .expect("32 bytes is a valid HKDF length");
        hkdf.expand(b"lumen/index/v1", &mut keys.index_key)
            .expect("32 bytes is a valid HKDF length");
        keys
    }

    /// The group's public name on the server.
    pub fn sync_id(&self) -> String {
        hex(&self.sync_id)
    }

    /// Sent as the bearer token. It proves membership but cannot decrypt anything.
    pub fn auth_token(&self) -> String {
        b64(&self.auth_key)
    }

    /// What the server stores in place of the token: SHA-256 of the token text.
    pub fn verifier(&self) -> String {
        hex(&Sha256::digest(self.auth_token().as_bytes()))
    }

    /// A stable, opaque id for a record, so the server can index it without knowing what it is.
    pub fn item_id(&self, kind: &str, key: &str) -> String {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.index_key)
            .expect("HMAC accepts any key length");
        mac.update(kind.as_bytes());
        mac.update(&[0]);
        mac.update(key.as_bytes());
        hex(&mac.finalize().into_bytes())[..32].to_string()
    }

    fn aad(&self, id: &str, updated_at: i64, deleted: bool) -> Vec<u8> {
        [
            self.sync_id().as_str(),
            id,
            &updated_at.to_string(),
            if deleted { "1" } else { "0" },
        ]
        .join("\0")
        .into_bytes()
    }

    pub fn seal(&self, item: &SyncItem) -> Result<SealedItem> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce)
            .map_err(|error| PlayerError::Io(format!("No secure randomness available: {error}")))?;
        self.seal_with_nonce(item, nonce)
    }

    fn seal_with_nonce(&self, item: &SyncItem, nonce: [u8; NONCE_LEN]) -> Result<SealedItem> {
        let id = self.item_id(&item.kind, &item.key);
        let plain = serde_json::to_vec(&Plain {
            kind: item.kind.clone(),
            key: item.key.clone(),
            payload: item.payload.clone(),
        })?;
        let aad = self.aad(&id, item.updated_at, item.deleted);
        let cipher = XChaCha20Poly1305::new((&self.enc_key).into());
        let sealed = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &plain,
                    aad: &aad,
                },
            )
            .map_err(|_| PlayerError::Io("Could not encrypt a sync record".into()))?;
        let mut blob = Vec::with_capacity(NONCE_LEN + sealed.len());
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&sealed);
        Ok(SealedItem {
            id,
            updated_at: item.updated_at,
            deleted: item.deleted,
            blob: b64(&blob),
            seq: None,
        })
    }

    /// Decrypts a record. Fails if it was not sealed with this group's keys, was altered, or was
    /// moved to another id or timestamp.
    pub fn open(&self, sealed: &SealedItem) -> Result<SyncItem> {
        let bad = || PlayerError::Parse("A sync record could not be verified".into());
        let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&sealed.blob)
            .map_err(|_| bad())?;
        if blob.len() <= NONCE_LEN {
            return Err(bad());
        }
        let (nonce, ciphertext) = blob.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| bad())?;
        let aad = self.aad(&sealed.id, sealed.updated_at, sealed.deleted);
        let cipher = XChaCha20Poly1305::new((&self.enc_key).into());
        let plain = cipher
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| bad())?;
        let plain: Plain = serde_json::from_slice(&plain).map_err(|_| bad())?;
        // The id is derived from the kind and key, so a record whose id does not match them was
        // tampered with even though it decrypted.
        if self.item_id(&plain.kind, &plain.key) != sealed.id {
            return Err(bad());
        }
        Ok(SyncItem {
            kind: plain.kind,
            key: plain.key,
            payload: plain.payload,
            updated_at: sealed.updated_at,
            deleted: sealed.deleted,
        })
    }
}

/// A fresh group secret from the operating system's random source.
pub fn generate_root() -> Result<[u8; ROOT_LEN]> {
    let mut root = [0u8; ROOT_LEN];
    getrandom::fill(&mut root)
        .map_err(|error| PlayerError::Io(format!("No secure randomness available: {error}")))?;
    Ok(root)
}

fn checksum(root: &[u8; ROOT_LEN]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"lumen/sync-key-check/v1");
    hasher.update(root);
    // Three bytes encode to five characters; the first four are the first 20 bits.
    BASE32.encode(&hasher.finalize()[..3])[..CHECK_LEN].to_string()
}

/// The text a person writes down: 56 characters in dash-separated groups of four.
pub fn format_key(root: &[u8; ROOT_LEN]) -> String {
    let raw = format!("{}{}", BASE32.encode(root), checksum(root));
    raw.as_bytes()
        .chunks(4)
        .map(|group| std::str::from_utf8(group).expect("base32 is ASCII"))
        .collect::<Vec<_>>()
        .join("-")
}

/// Reads a typed or pasted key. Case, dashes and spaces do not matter, and the look-alikes
/// `O`, `I` and `L` are read as `0`, `1` and `1`. A wrong checksum is refused, so a typo cannot
/// quietly create a second, empty group.
pub fn parse_key(text: &str) -> Result<[u8; ROOT_LEN]> {
    let invalid = || {
        PlayerError::InvalidArg("That sync key is not valid. Check it for typing mistakes.".into())
    };
    let cleaned: String = text
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-')
        .map(|ch| match ch.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        })
        .collect();
    if cleaned.len() != KEY_ENCODED_LEN + CHECK_LEN || !cleaned.is_ascii() {
        return Err(invalid());
    }
    let (body, check) = cleaned.split_at(KEY_ENCODED_LEN);
    let decoded = BASE32.decode(body.as_bytes()).map_err(|_| invalid())?;
    let root: [u8; ROOT_LEN] = decoded.try_into().map_err(|_| invalid())?;
    if checksum(&root) != check {
        return Err(invalid());
    }
    Ok(root)
}

fn leading_zero_bits(hash: &[u8]) -> u32 {
    let mut bits = 0;
    for byte in hash {
        if *byte == 0 {
            bits += 8;
        } else {
            return bits + byte.leading_zeros();
        }
    }
    bits
}

fn pow_hash(sync_id: &str, nonce: u64) -> [u8; 32] {
    Sha256::digest(format!("{sync_id}:{nonce}").as_bytes()).into()
}

/// Whether `nonce` is a valid proof of work for registering `sync_id`.
pub fn pow_valid(sync_id: &str, nonce: u64, bits: u32) -> bool {
    leading_zero_bits(&pow_hash(sync_id, nonce)) >= bits
}

/// Finds a nonce for `sync_id`. About a million hashes at `POW_BITS`; run it off the async runtime.
pub fn solve_pow(sync_id: &str, bits: u32) -> u64 {
    (0u64..)
        .find(|nonce| pow_valid(sync_id, *nonce, bits))
        .expect("a nonce exists")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Vectors below were computed independently in Python (hashlib/hmac for HKDF and the
    /// derived values, libsodium via PyNaCl for XChaCha20-Poly1305) from this root.
    fn root() -> [u8; 32] {
        let mut root = [0u8; 32];
        for (index, byte) in root.iter_mut().enumerate() {
            *byte = index as u8;
        }
        root
    }

    fn item() -> SyncItem {
        SyncItem {
            kind: "favorite".into(),
            key: "p1:abc".into(),
            payload: json!({"url": "http://x.example/a"}),
            updated_at: 1_700_000_000_000,
            deleted: false,
        }
    }

    #[test]
    fn derived_values_match_the_independent_computation() {
        let keys = SyncKeys::from_root(&root());
        assert_eq!(keys.sync_id(), "120c67b7bdb6cb65268c0b7557e759ee");
        assert_eq!(
            keys.auth_token(),
            "10Zh2dt2EfIdiaDfS__HMHPX8fe-i-lxDsnLG81uxf4"
        );
        assert_eq!(
            keys.verifier(),
            "490b64de76fdfb13527d360d5e628d62bb51decf0b2783f540cec9fc11b186a4"
        );
        assert_eq!(
            keys.item_id("favorite", "p1:abc"),
            "6c694b58ee773e51b7ef4bdffc6a2d05"
        );
    }

    #[test]
    fn a_sealed_record_matches_libsodium_byte_for_byte() {
        let keys = SyncKeys::from_root(&root());
        let mut nonce = [0u8; 24];
        for (index, byte) in nonce.iter_mut().enumerate() {
            *byte = index as u8;
        }
        let sealed = keys.seal_with_nonce(&item(), nonce).unwrap();
        assert_eq!(
            sealed.blob,
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXNaDuQvUncrYA1TbV-d0zZopKbiSTgDdatxoXU3YFTDKvxuyp5fLxUC_KHIn9XmGNoFmyey9K55sy9TbOzoygXxiXNest6cAQtVo1HN5MuumpZsztmnjI97w"
        );
        assert_eq!(sealed.id, "6c694b58ee773e51b7ef4bdffc6a2d05");
    }

    #[test]
    fn a_record_survives_a_round_trip_and_hides_its_content() {
        let keys = SyncKeys::from_root(&root());
        let sealed = keys.seal(&item()).unwrap();
        let opened = keys.open(&sealed).unwrap();
        assert_eq!(opened.kind, "favorite");
        assert_eq!(opened.key, "p1:abc");
        assert_eq!(opened.payload, json!({"url": "http://x.example/a"}));
        assert_eq!(opened.updated_at, 1_700_000_000_000);

        let wire = serde_json::to_string(&sealed).unwrap();
        for secret in ["x.example", "favorite", "p1:abc"] {
            assert!(
                !wire.contains(secret),
                "{secret} must not appear on the wire: {wire}"
            );
        }
        assert_ne!(
            keys.seal(&item()).unwrap().blob,
            sealed.blob,
            "a fresh nonce each time"
        );
    }

    #[test]
    fn tampering_and_misuse_are_detected() {
        let keys = SyncKeys::from_root(&root());
        let sealed = keys.seal(&item()).unwrap();

        let mut flipped = sealed.clone();
        let mut bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&flipped.blob)
            .unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        flipped.blob = b64(&bytes);
        assert!(keys.open(&flipped).is_err(), "a changed byte");

        let mut later = sealed.clone();
        later.updated_at += 1;
        assert!(keys.open(&later).is_err(), "a changed timestamp");

        let mut undeleted = sealed.clone();
        undeleted.deleted = true;
        assert!(keys.open(&undeleted).is_err(), "a changed deleted flag");

        let other = keys
            .seal(&SyncItem {
                key: "p1:other".into(),
                ..item()
            })
            .unwrap();
        let swapped = SealedItem {
            blob: other.blob.clone(),
            ..sealed.clone()
        };
        assert!(
            keys.open(&swapped).is_err(),
            "a blob moved to another item's id"
        );

        let mut stranger_root = root();
        stranger_root[0] ^= 0xff;
        assert!(
            SyncKeys::from_root(&stranger_root).open(&sealed).is_err(),
            "another group's key"
        );

        let mut truncated = sealed.clone();
        truncated.blob = "AAAA".into();
        assert!(keys.open(&truncated).is_err());
        truncated.blob = "!!!not base64!!!".into();
        assert!(keys.open(&truncated).is_err());
    }

    #[test]
    fn the_key_string_matches_the_independent_format_and_round_trips() {
        let text = format_key(&root());
        assert_eq!(
            text,
            "000G-40R4-0M30-E209-185G-R38E-1W81-24GK-2GAH-C5RR-34D1-P70X-3RFG-TTB4"
        );
        assert_eq!(parse_key(&text).unwrap(), root());

        let random = generate_root().unwrap();
        assert_eq!(parse_key(&format_key(&random)).unwrap(), random);
        assert_ne!(random, generate_root().unwrap(), "roots are random");
    }

    #[test]
    fn the_key_string_forgives_formatting_but_not_mistakes() {
        let text = format_key(&root());
        let lowered = text.to_lowercase().replace('-', " ");
        assert_eq!(
            parse_key(&lowered).unwrap(),
            root(),
            "case, spaces and dashes do not matter"
        );
        assert_eq!(parse_key(&format!("  {text}\n")).unwrap(), root());

        // `0` typed as `O` and `1` typed as `I` or `L` read back correctly.
        let confusable = text.replace('0', "O").replace('1', "l");
        assert_eq!(parse_key(&confusable).unwrap(), root());

        // A single wrong character is caught by the checksum, wherever it is.
        let chars: Vec<char> = text.chars().collect();
        for position in [0, 5, 20, 40, 65, 68] {
            let mut typo = chars.clone();
            typo[position] = if typo[position] == '2' { '3' } else { '2' };
            let typo: String = typo.into_iter().collect();
            assert!(
                parse_key(&typo).is_err(),
                "a typo at {position} must be rejected"
            );
        }
        assert!(parse_key("").is_err());
        assert!(parse_key("0000-0000").is_err(), "too short");
        assert!(parse_key(&format!("{text}0")).is_err(), "too long");
        assert!(
            parse_key(&text.replace('0', "U")).is_err(),
            "U is not in the alphabet"
        );
    }

    #[test]
    fn proof_of_work_matches_the_independent_computation() {
        let sync_id = "120c67b7bdb6cb65268c0b7557e759ee";
        assert!(
            pow_valid(sync_id, 6369, 12),
            "the nonce found independently is valid"
        );
        assert!(!pow_valid(sync_id, 6370, 12));
        assert_eq!(solve_pow(sync_id, 12), 6369, "and it is the smallest one");
        assert!(!pow_valid(sync_id, 6369, 20), "difficulty is enforced");
        assert_eq!(leading_zero_bits(&[0x00, 0x1f]), 11);
        assert_eq!(leading_zero_bits(&[0x80]), 0);
    }
}

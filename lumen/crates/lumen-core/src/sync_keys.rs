//! The sync key: where it is kept, and turning sync on, joining from another device, showing the
//! key and turning sync off.
//!
//! There are no accounts. The 32-byte root secret lives in the OS keychain (never in the
//! settings table), and every other key is derived from it (`sync_crypto`). Anyone holding the
//! key text can read and write the synced data, and nobody can recover it for you.

use std::sync::Mutex;

use serde::Serialize;
use sqlx::{Pool, Sqlite};

use crate::db::{delete_setting, get_setting, set_setting};
use crate::error::{PlayerError, Result};
use crate::sync::{self, Remote};
use crate::sync_crypto::{self, SyncKeys};

const KEYCHAIN_SERVICE: &str = "dev.lumen.app.sync";
const KEYCHAIN_ACCOUNT: &str = "sync-key";
const SERVER_SETTING: &str = "sync_server_url";

/// The sync server used when the user has not chosen one. Set when building a release with
/// `LUMEN_SYNC_URL=https://...`; a development build has none and asks for an address.
const DEFAULT_SERVER: Option<&str> = option_env!("LUMEN_SYNC_URL");

/// Where the root secret is kept. The OS keychain in the app; memory in tests.
pub trait RootStore: Send + Sync {
    fn load(&self) -> Result<Option<[u8; 32]>>;
    fn save(&self, root: &[u8; 32]) -> Result<()>;
    fn clear(&self) -> Result<()>;
}

pub struct KeyringStore;

fn keychain_error(error: keyring::Error) -> PlayerError {
    PlayerError::Io(format!("OS keychain unavailable: {error}"))
}

fn entry() -> Result<keyring::Entry> {
    keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT).map_err(keychain_error)
}

impl RootStore for KeyringStore {
    fn load(&self) -> Result<Option<[u8; 32]>> {
        match entry()?.get_password() {
            Ok(text) => sync_crypto::parse_key(&text).map(Some),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(keychain_error(error)),
        }
    }

    fn save(&self, root: &[u8; 32]) -> Result<()> {
        entry()?.set_password(&sync_crypto::format_key(root)).map_err(keychain_error)
    }

    fn clear(&self) -> Result<()> {
        match entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(keychain_error(error)),
        }
    }
}

/// Earlier builds signed in to a hosted account and kept its token in the keychain. That sign-in
/// no longer exists, so the leftover token is removed (best effort; there is nothing to report).
pub fn clear_legacy_account_token() {
    if let Ok(entry) = keyring::Entry::new("dev.lumen.app.account", "app-token") {
        let _ = entry.delete_credential();
    }
}

/// A store that keeps the root in memory.
#[derive(Default)]
pub struct MemoryStore(Mutex<Option<[u8; 32]>>);

impl RootStore for MemoryStore {
    fn load(&self) -> Result<Option<[u8; 32]>> {
        Ok(*self.0.lock().expect("store lock"))
    }

    fn save(&self, root: &[u8; 32]) -> Result<()> {
        *self.0.lock().expect("store lock") = Some(*root);
        Ok(())
    }

    fn clear(&self) -> Result<()> {
        *self.0.lock().expect("store lock") = None;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SyncState {
    /// This device holds a sync key and syncs.
    pub enabled: bool,
    /// The server in use: the one the user set, otherwise the built-in default, otherwise none.
    pub server_url: Option<String>,
    /// Whether `server_url` is the built-in default (so the UI can offer to change it).
    pub default_server: bool,
}

/// Checks a sync server address and returns it without a trailing slash. The bearer token goes
/// to this address, so it must be https; plain http is allowed only to this machine, for a
/// development server.
pub fn parse_server_url(raw: &str) -> Result<String> {
    let invalid = |why: &str| PlayerError::InvalidArg(format!("That sync server address is not valid: {why}"));
    let url = url::Url::parse(raw.trim()).map_err(|_| invalid("it is not a web address"))?;
    if url.query().is_some() || url.fragment().is_some() || !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("remove anything after the host name"));
    }
    if url.path() != "/" {
        return Err(invalid("use just the host name, such as https://sync.example.com"));
    }
    let loopback = match url.host() {
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => return Err(invalid("it has no host")),
    };
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        "http" => return Err(invalid("it must start with https://")),
        _ => return Err(invalid("it must start with https://")),
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

async fn server_url(pool: &Pool<Sqlite>) -> Result<(Option<String>, bool)> {
    if let Some(saved) = get_setting(pool, SERVER_SETTING).await?.filter(|value| !value.is_empty()) {
        return Ok((Some(saved), false));
    }
    Ok((DEFAULT_SERVER.map(str::to_string), true))
}

async fn require_server(pool: &Pool<Sqlite>) -> Result<String> {
    server_url(pool).await?.0.ok_or_else(|| {
        PlayerError::InvalidArg("No sync server is set. Enter the address of a Lumen sync server first".into())
    })
}

pub async fn state(pool: &Pool<Sqlite>) -> Result<SyncState> {
    state_with(pool, &KeyringStore).await
}

pub(crate) async fn state_with(pool: &Pool<Sqlite>, store: &dyn RootStore) -> Result<SyncState> {
    let (server_url, default_server) = server_url(pool).await?;
    Ok(SyncState { enabled: store.load()?.is_some(), server_url, default_server })
}

/// Saves the server address (an empty string goes back to the default). Refused while sync is on:
/// the key belongs to one server's group, so switching would silently strand the data.
pub async fn set_server(pool: &Pool<Sqlite>, raw: &str) -> Result<SyncState> {
    set_server_with(pool, &KeyringStore, raw).await
}

pub(crate) async fn set_server_with(pool: &Pool<Sqlite>, store: &dyn RootStore, raw: &str) -> Result<SyncState> {
    if store.load()?.is_some() {
        return Err(PlayerError::InvalidArg("Turn sync off before changing the sync server".into()));
    }
    if raw.trim().is_empty() {
        delete_setting(pool, SERVER_SETTING).await?;
    } else {
        set_setting(pool, SERVER_SETTING, &parse_server_url(raw)?).await?;
    }
    state_with(pool, store).await
}

/// The remote for the key on this device, or `None` while sync is off.
pub(crate) async fn remote(pool: &Pool<Sqlite>, store: &dyn RootStore) -> Result<Option<Remote>> {
    let Some(root) = store.load()? else {
        return Ok(None);
    };
    let origin = require_server(pool).await?;
    Remote::new(&origin, SyncKeys::from_root(&root)).map(Some)
}

/// Turns sync on with a new key and returns the key text to show the user. The server group is
/// registered first and the key is kept only once that worked, so a failed attempt leaves
/// nothing behind.
pub async fn enable(pool: &Pool<Sqlite>) -> Result<String> {
    enable_with(pool, &KeyringStore, sync_crypto::POW_BITS).await
}

pub(crate) async fn enable_with(pool: &Pool<Sqlite>, store: &dyn RootStore, pow_bits: u32) -> Result<String> {
    if store.load()?.is_some() {
        return Err(PlayerError::InvalidArg("Sync is already on".into()));
    }
    let origin = require_server(pool).await?;
    let root = sync_crypto::generate_root()?;
    let remote = Remote::new(&origin, SyncKeys::from_root(&root))?;
    remote.register(pow_bits).await?;
    store.save(&root)?;
    // A fresh group: everything already on this device goes up on the first sync.
    sync::forget(pool).await?;
    Ok(sync_crypto::format_key(&root))
}

/// Turns sync on with a key from another device. Nothing is created: if the server has no group
/// for the key, this fails instead of starting an empty one.
pub async fn join(pool: &Pool<Sqlite>, key_text: &str) -> Result<()> {
    join_with(pool, &KeyringStore, key_text).await
}

pub(crate) async fn join_with(pool: &Pool<Sqlite>, store: &dyn RootStore, key_text: &str) -> Result<()> {
    if store.load()?.is_some() {
        return Err(PlayerError::InvalidArg("Sync is already on".into()));
    }
    let root = sync_crypto::parse_key(key_text)?;
    let origin = require_server(pool).await?;
    let remote = Remote::new(&origin, SyncKeys::from_root(&root))?;
    if !remote.exists().await? {
        return Err(PlayerError::InvalidArg(
            "No synced data was found for that key on this server. Check the key and the server address".into(),
        ));
    }
    store.save(&root)?;
    sync::forget(pool).await?;
    Ok(())
}

/// The key text of this device's group, for adding another device.
pub fn show_key() -> Result<String> {
    show_key_with(&KeyringStore)
}

pub(crate) fn show_key_with(store: &dyn RootStore) -> Result<String> {
    let root = store.load()?.ok_or_else(|| PlayerError::InvalidArg("Sync is off".into()))?;
    Ok(sync_crypto::format_key(&root))
}

/// Turns sync off on this device: the key is forgotten, and so is the queue of unsent changes.
/// Local favorites, history and profiles stay. With `delete_remote` the synced copy is also
/// erased from the server for every device; if that fails, nothing is changed, so the user can
/// try again with the key still in place.
pub async fn disable(pool: &Pool<Sqlite>, delete_remote: bool) -> Result<()> {
    disable_with(pool, &KeyringStore, delete_remote).await
}

pub(crate) async fn disable_with(pool: &Pool<Sqlite>, store: &dyn RootStore, delete_remote: bool) -> Result<()> {
    if delete_remote
        && let Some(remote) = remote(pool, store).await?
    {
        remote.delete_group().await?;
    }
    store.clear()?;
    sync::forget(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_addresses_must_be_https_except_on_this_machine() {
        assert_eq!(parse_server_url("https://sync.example.com").unwrap(), "https://sync.example.com");
        assert_eq!(parse_server_url("  https://sync.example.com/ ").unwrap(), "https://sync.example.com");
        assert_eq!(parse_server_url("http://127.0.0.1:8787").unwrap(), "http://127.0.0.1:8787");
        assert_eq!(parse_server_url("http://localhost:8787").unwrap(), "http://localhost:8787");
        assert!(parse_server_url("http://sync.example.com").is_err(), "the bearer token must not cross plain http");
        assert!(parse_server_url("http://192.168.1.5").is_err(), "a LAN address is not this machine");
        assert!(parse_server_url("ftp://sync.example.com").is_err());
        assert!(parse_server_url("sync.example.com").is_err());
        assert!(parse_server_url("https://user:pw@sync.example.com").is_err(), "no credentials in the address");
        assert!(parse_server_url("https://sync.example.com/api?x=1").is_err());
        assert!(parse_server_url("https://sync.example.com/api").is_err());
    }
}

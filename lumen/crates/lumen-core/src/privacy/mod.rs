//! Network privacy: an optional local proxy relay that all of Lumen's own outbound traffic
//! (metadata requests today; mpv and yt-dlp in later phases) is routed through, which in turn
//! forwards to a SOCKS5 or HTTP proxy the user configures (their own VPN, Tor, Shadowsocks,
//! ...). Lumen never talks to that upstream proxy directly except through this module; see
//! `relay` for the local server and `upstream` for the two proxy-client protocols it speaks.
//!
//! This is fail-closed throughout: when privacy is meant to be on but the relay could not start,
//! or has not finished starting yet, every guarded network call refuses outright rather than
//! silently going direct. The only direct connections are the deliberate local-network bypass
//! (handled inside the relay) and a handful of call sites that are explicitly local-only (device
//! casting), which never route through this module at all.

pub mod relay;
pub mod upstream;

use std::net::SocketAddr;
use std::sync::RwLock;

use sqlx::{Pool, Sqlite};

use crate::db::get_setting;
use crate::error::{PlayerError, Result};

pub const ENABLED_SETTING: &str = "network_privacy_enabled";
pub const UPSTREAM_SETTING: &str = "network_privacy_upstream";
pub const USERNAME_SETTING: &str = "network_privacy_username";

/// A parsed, validated upstream proxy address. Credentials are kept separately (passed at
/// connect time from the OS keychain) so they never sit in a value that might be logged.
#[derive(Debug, Clone, PartialEq)]
pub struct Upstream {
    pub scheme: UpstreamScheme,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamScheme {
    Socks5,
    Http,
}

/// Parses and validates a user-supplied upstream proxy address. Accepts `socks5://`,
/// `socks5h://` (treated the same: both always resolve the target remotely, never locally) and
/// `http://`, each with an explicit host and port and nothing else. `socks4`, `socks4a` and
/// `https` (TLS to the proxy itself) are refused by name, since they are not supported yet,
/// rather than silently mishandled.
pub fn parse_upstream(raw: &str) -> Result<Upstream> {
    let trimmed = raw.trim();
    let url = url::Url::parse(trimmed)
        .map_err(|_| PlayerError::InvalidArg("That is not a valid proxy address".into()))?;
    let scheme = match url.scheme() {
        "socks5" | "socks5h" => UpstreamScheme::Socks5,
        "http" => UpstreamScheme::Http,
        "socks4" | "socks4a" => {
            return Err(PlayerError::InvalidArg(
                "SOCKS4 is not supported; use a SOCKS5 or HTTP proxy".into(),
            ));
        }
        "https" => {
            return Err(PlayerError::InvalidArg(
                "A proxy reached over its own TLS connection is not supported yet".into(),
            ));
        }
        other => {
            return Err(PlayerError::InvalidArg(format!(
                "\"{other}://\" is not a supported proxy address"
            )));
        }
    };
    let host = url
        .host_str()
        .ok_or_else(|| PlayerError::InvalidArg("The proxy address needs a host".into()))?
        .to_string();
    let port = url.port().ok_or_else(|| {
        PlayerError::InvalidArg("Include the port, for example socks5://127.0.0.1:9050".to_string())
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(PlayerError::InvalidArg(
            "Enter the username and password in their own fields, not in the address".into(),
        ));
    }
    let path = url.path();
    if !(path.is_empty() || path == "/") || url.query().is_some() || url.fragment().is_some() {
        return Err(PlayerError::InvalidArg(
            "The proxy address should be just scheme://host:port".into(),
        ));
    }
    Ok(Upstream {
        scheme,
        host,
        port,
        username: None,
    })
}

/// Where Lumen's own network traffic currently goes.
#[derive(Debug, Clone)]
pub enum Route {
    /// Network privacy is off: connect directly, subject to the usual `netguard` checks.
    Direct,
    /// Starting up: the real setting has not been read yet. Guarded calls refuse rather than
    /// risk a brief window of going direct while privacy was meant to be on.
    Pending,
    /// Privacy is on and the local relay is listening here.
    Relay {
        local_addr: SocketAddr,
        token: String,
    },
    /// Privacy is on but the relay is not usable (it could not bind, the keychain could not be
    /// read, or the saved upstream address no longer parses). Holds why, for the user.
    Unavailable(String),
}

// Defaults to `Direct`, not `Pending`: this is a library default for every consumer of
// `lumen-core` (including its own test suite, which never touches this module), and must match
// today's behavior — connect directly — until something explicitly opts in. The `Pending`
// fail-closed window is a Tauri-side startup policy: `src-tauri`'s own setup sets it before
// reading settings, then resolves it to `Direct`, `Relay` or `Unavailable`.
static ROUTE: RwLock<Route> = RwLock::new(Route::Direct);

pub fn set_route(route: Route) {
    *ROUTE.write().unwrap_or_else(|p| p.into_inner()) = route;
}

pub fn current_route() -> Route {
    ROUTE.read().unwrap_or_else(|p| p.into_inner()).clone()
}

/// The running relay, if network privacy is on. Held here (not in `src-tauri`) so the whole
/// subsystem — settings, relay, route — stays in one place.
static RELAY_HANDLE: tokio::sync::Mutex<Option<relay::Relay>> = tokio::sync::Mutex::const_new(None);

/// Reads the network-privacy settings from `pool` and the keychain, and starts, updates or stops
/// the local relay to match, setting the global route accordingly. Safe to call repeatedly — at
/// startup, and again whenever a relevant setting changes — and never leaves the route stuck on
/// `Pending`: every path below ends by setting it to `Direct`, `Relay` or `Unavailable`.
pub async fn reconcile(pool: &Pool<Sqlite>, allow_lan: bool) {
    let enabled = matches!(
        get_setting(pool, ENABLED_SETTING).await,
        Ok(Some(value)) if value == "true"
    );
    if !enabled {
        *RELAY_HANDLE.lock().await = None;
        set_route(Route::Direct);
        return;
    }

    let parsed = get_setting(pool, UPSTREAM_SETTING)
        .await
        .ok()
        .flatten()
        .and_then(|raw| parse_upstream(&raw).ok());
    let Some(mut parsed) = parsed else {
        *RELAY_HANDLE.lock().await = None;
        set_route(Route::Unavailable(
            "no valid proxy address is saved".to_string(),
        ));
        return;
    };
    let username = get_setting(pool, USERNAME_SETTING)
        .await
        .ok()
        .flatten()
        .filter(|value| !value.is_empty());
    parsed.username = username;
    // A keychain that cannot be read here is treated as "no password" rather than a hard
    // failure: `network_privacy_set_cmd` already surfaced any real keychain error when the
    // password was saved, so a transient read failure should not take a working relay down.
    let password = crate::secrets::load_proxy_password().ok().flatten();

    let mut guard = RELAY_HANDLE.lock().await;
    match guard.as_ref() {
        Some(relay) => {
            relay.set_upstream(parsed, password);
            relay.set_allow_lan(allow_lan);
            set_route(Route::Relay {
                local_addr: relay.local_addr,
                token: relay.token.clone(),
            });
        }
        None => match relay::Relay::start(parsed, password, allow_lan).await {
            Ok(relay) => {
                set_route(Route::Relay {
                    local_addr: relay.local_addr,
                    token: relay.token.clone(),
                });
                *guard = Some(relay);
            }
            Err(error) => set_route(Route::Unavailable(String::from(error))),
        },
    }
}

/// Updates the local-network bypass on a running relay, if any. Called whenever the "allow
/// sources on my local network" setting changes, so the relay does not need `reconcile`'s full
/// settings re-read just for this.
pub async fn set_allow_lan(allow_lan: bool) {
    if let Some(relay) = RELAY_HANDLE.lock().await.as_ref() {
        relay.set_allow_lan(allow_lan);
    }
}

/// Verifies an upstream proxy actually works, without starting the relay: opens a connection
/// through it to a host Lumen already trusts (the same one self-update checks against) and
/// drops it at once.
pub async fn test_upstream(upstream: &Upstream, password: Option<&str>) -> Result<()> {
    relay::test_connect(upstream, password).await
}

fn unavailable_error(reason: &str) -> PlayerError {
    PlayerError::Http(format!("Network privacy proxy is not available: {reason}"))
}

/// Applies the current route to a `reqwest::ClientBuilder`. `Direct` passes the builder through
/// unchanged; `Relay` adds the local relay as an HTTP proxy (reqwest needs no extra feature for
/// this, since the relay itself is what may speak SOCKS5 to the real upstream); `Pending` and
/// `Unavailable` refuse rather than ever fall back to a direct connection.
pub fn apply_with(
    builder: reqwest::ClientBuilder,
    route: &Route,
) -> Result<reqwest::ClientBuilder> {
    match proxy_url_for(route)? {
        None => Ok(builder),
        Some(proxy_url) => {
            let proxy = reqwest::Proxy::all(&proxy_url)
                .map_err(|e| PlayerError::Http(format!("Could not use the local proxy: {e}")))?;
            Ok(builder.proxy(proxy))
        }
    }
}

pub fn apply(builder: reqwest::ClientBuilder) -> Result<reqwest::ClientBuilder> {
    apply_with(builder, &current_route())
}

/// The proxy URL for the given route, or `None` when it is `Direct` — the same mapping
/// `apply_with` uses, shared so `apply_with` and `child_proxy_url` cannot drift apart.
fn proxy_url_for(route: &Route) -> Result<Option<String>> {
    match route {
        Route::Direct => Ok(None),
        Route::Pending => Err(unavailable_error("still starting up")),
        Route::Unavailable(reason) => Err(unavailable_error(reason)),
        Route::Relay { local_addr, token } => {
            Ok(Some(format!("http://lumen:{token}@{local_addr}")))
        }
    }
}

/// The proxy URL to hand to a child process (mpv, yt-dlp), or `None` when privacy is off.
/// Errors the same way `apply` does when privacy is meant to be on but is not usable.
pub fn child_proxy_url() -> Result<Option<String>> {
    proxy_url_for(&current_route())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_upstream_addresses_parse() {
        let socks = parse_upstream("socks5://127.0.0.1:9050").unwrap();
        assert_eq!(socks.scheme, UpstreamScheme::Socks5);
        assert_eq!(socks.host, "127.0.0.1");
        assert_eq!(socks.port, 9050);

        let socks5h = parse_upstream("socks5h://proxy.example:1080").unwrap();
        assert_eq!(
            socks5h.scheme,
            UpstreamScheme::Socks5,
            "socks5h is treated like socks5"
        );

        let http = parse_upstream("http://10.0.0.5:3128").unwrap();
        assert_eq!(http.scheme, UpstreamScheme::Http);

        assert_eq!(
            parse_upstream("  socks5://127.0.0.1:9050  ").unwrap(),
            socks,
            "surrounding whitespace is trimmed"
        );
    }

    #[test]
    fn invalid_upstream_addresses_are_refused() {
        for bad in [
            "not a url",
            "ftp://127.0.0.1:21",
            "socks4://127.0.0.1:1080",
            "socks4a://127.0.0.1:1080",
            "https://127.0.0.1:443",
            "socks5://127.0.0.1",                // no port
            "socks5://:9050",                    // no host
            "socks5://user:pass@127.0.0.1:9050", // credentials belong in their own fields
            "socks5://127.0.0.1:9050/some/path",
            "socks5://127.0.0.1:9050?x=1",
            "socks5://127.0.0.1:9050#frag",
        ] {
            assert!(parse_upstream(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn apply_with_direct_passes_the_builder_through_unchanged() {
        let builder = reqwest::Client::builder();
        assert!(apply_with(builder, &Route::Direct).is_ok());
    }

    #[test]
    fn apply_with_pending_and_unavailable_refuse_rather_than_go_direct() {
        let builder = reqwest::Client::builder();
        assert!(apply_with(builder, &Route::Pending).is_err());
        let builder = reqwest::Client::builder();
        assert!(apply_with(builder, &Route::Unavailable("test".into())).is_err());
    }

    #[test]
    fn proxy_url_for_matches_each_route() {
        assert_eq!(proxy_url_for(&Route::Direct).unwrap(), None);
        assert!(proxy_url_for(&Route::Pending).is_err());
        assert!(proxy_url_for(&Route::Unavailable("down".into())).is_err());
        assert_eq!(
            proxy_url_for(&Route::Relay {
                local_addr: "127.0.0.1:9".parse().unwrap(),
                token: "tok".into(),
            })
            .unwrap(),
            Some("http://lumen:tok@127.0.0.1:9".to_string())
        );
    }

    #[test]
    fn apply_with_relay_adds_the_local_proxy() {
        let builder = reqwest::Client::builder();
        let route = Route::Relay {
            local_addr: "127.0.0.1:12345".parse().unwrap(),
            token: "abc123".into(),
        };
        let builder = apply_with(builder, &route).unwrap();
        // There is no public getter for a configured proxy on `ClientBuilder`; building
        // successfully (and relay.rs's own tests proving traffic actually reaches the relay) is
        // the practical check here.
        assert!(builder.build().is_ok());
    }
}

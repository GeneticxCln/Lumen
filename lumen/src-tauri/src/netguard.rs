//! Keeps requests the user did not write out of private networks.
//!
//! Playlist URLs, harvested links and provider endpoints are text from the internet. Fetching
//! one blindly lets a hostile page aim the app at `http://127.0.0.1:…`, a router admin page or a
//! cloud metadata address. Three places need checking, because each can be the one that slips:
//!
//! 1. the URL the caller typed (`validate_url`),
//! 2. every redirect hop (`redirect_policy`), and
//! 3. the address a hostname actually resolves to when the socket is opened
//!    (`PublicOnlyResolver`). Checking only a lookup done earlier leaves a window for DNS
//!    rebinding, where the name answers with a public address first and a private one second.
//!
//! reqwest never consults the resolver for an IP-literal host, so (1) and (2) check literals.
//!
//! Local-network sources (a playlist on a NAS, Jellyfin on the same machine) are legitimate, so
//! `allow_lan` permits private ranges and loopback. It never permits unspecified, multicast,
//! broadcast or the cloud metadata address.

use crate::error::{PlayerError, Result};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use url::{Host, Url};

const MAX_REDIRECTS: usize = 5;

fn is_blocked_always_v4(ip: Ipv4Addr) -> bool {
    ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
        // Cloud metadata endpoint: never a media source, and where credentials live.
        || ip == Ipv4Addr::new(169, 254, 169, 254)
        // Reserved 240.0.0.0/4.
        || ip.octets()[0] >= 240
}

fn is_lan_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        // Carrier-grade NAT 100.64.0.0/10.
        || (a == 100 && (64..128).contains(&b))
        // Benchmarking 198.18.0.0/15 and IETF protocol assignments 192.0.0.0/24.
        || (a == 198 && (b == 18 || b == 19))
        || (a == 192 && b == 0 && ip.octets()[2] == 0)
        // Documentation ranges.
        || ip.is_documentation()
        // "This network" 0.0.0.0/8.
        || a == 0
}

fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return Some(v4);
    }
    // NAT64 64:ff9b::/96 embeds an IPv4 address in the last 32 bits.
    let segments = ip.segments();
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let [a, b] = segments[6].to_be_bytes();
        let [c, d] = segments[7].to_be_bytes();
        return Some(Ipv4Addr::new(a, b, c, d));
    }
    None
}

fn is_lan_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    ip.is_loopback()
        // Unique local fc00::/7 and link-local fe80::/10.
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        // Documentation 2001:db8::/32.
        || (first == 0x2001 && ip.segments()[1] == 0x0db8)
}

/// Whether a request to `ip` is allowed. `allow_lan` additionally permits private and loopback
/// ranges; the always-blocked ranges stay blocked either way.
pub fn is_allowed_ip(ip: IpAddr, allow_lan: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => !is_blocked_always_v4(v4) && (allow_lan || !is_lan_v4(v4)),
        IpAddr::V6(v6) => {
            if let Some(v4) = embedded_v4(v6) {
                return is_allowed_ip(IpAddr::V4(v4), allow_lan);
            }
            !v6.is_unspecified() && !v6.is_multicast() && (allow_lan || !is_lan_v6(v6))
        }
    }
}

fn host_error(url: &Url, reason: &str) -> PlayerError {
    // Say which host, never the whole URL: it may carry a token.
    PlayerError::InvalidArg(format!(
        "{} {reason}",
        url.host_str().unwrap_or("That address")
    ))
}

/// Checks the parts of a URL that can be judged without a lookup: scheme, host presence, and
/// IP literals. Hostnames are checked again at connect time by `PublicOnlyResolver`.
pub fn check_url(url: &Url, allow_lan: bool) -> Result<()> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(PlayerError::InvalidArg("Only http:// and https:// addresses are supported".into()));
    }
    match url.host() {
        None => Err(PlayerError::InvalidArg("The address has no host".into())),
        Some(Host::Ipv4(ip)) if !is_allowed_ip(IpAddr::V4(ip), allow_lan) => Err(host_error(
            url,
            "is a private or local address. Turn on local-network sources in Settings to use it.",
        )),
        Some(Host::Ipv6(ip)) if !is_allowed_ip(IpAddr::V6(ip), allow_lan) => Err(host_error(
            url,
            "is a private or local address. Turn on local-network sources in Settings to use it.",
        )),
        Some(Host::Domain(name)) if !allow_lan && is_local_name(name) => Err(host_error(
            url,
            "is a local address. Turn on local-network sources in Settings to use it.",
        )),
        Some(_) => Ok(()),
    }
}

fn is_local_name(name: &str) -> bool {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    name == "localhost" || name.ends_with(".localhost") || name.ends_with(".local") || name.ends_with(".internal")
}

/// Parses and checks a user-supplied URL.
pub fn validate_url(raw: &str, allow_lan: bool) -> Result<Url> {
    let url = Url::parse(raw.trim()).map_err(|_| PlayerError::InvalidArg("That is not a valid web address".into()))?;
    check_url(&url, allow_lan)?;
    Ok(url)
}

/// Follows at most `MAX_REDIRECTS` hops and checks each one, so a public page cannot bounce the
/// request to a private address.
pub fn redirect_policy(allow_lan: bool) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        match check_url(attempt.url(), allow_lan) {
            Ok(()) => attempt.follow(),
            Err(error) => attempt.error(String::from(error)),
        }
    })
}

/// Resolves names and drops every address that is not allowed. If nothing is left, the connection
/// fails instead of falling back to a private address.
pub struct PublicOnlyResolver {
    allow_lan: bool,
}

impl Resolve for PublicOnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow_lan = self.allow_lan;
        Box::pin(async move {
            let host = name.as_str().to_string();
            let resolved: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let allowed: Vec<SocketAddr> = resolved
                .into_iter()
                .filter(|addr| is_allowed_ip(addr.ip(), allow_lan))
                .collect();
            if allowed.is_empty() {
                let reason = format!("{host} resolves to a private or local address");
                return Err(Box::<dyn std::error::Error + Send + Sync>::from(reason));
            }
            let addrs: Addrs = Box::new(allowed.into_iter());
            Ok(addrs)
        })
    }
}

/// A client builder wired with all three checks. Callers add their own timeout and user agent.
pub fn guarded_client(allow_lan: bool) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .dns_resolver(Arc::new(PublicOnlyResolver { allow_lan }))
        .redirect(redirect_policy(allow_lan))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn public_addresses_pass_and_private_ones_do_not() {
        for public in ["8.8.8.8", "93.184.216.34", "2606:4700:4700::1111"] {
            assert!(is_allowed_ip(ip(public), false), "{public} is public");
        }
        for private in [
            "127.0.0.1", "10.1.2.3", "172.16.0.9", "192.168.1.20", "169.254.1.1", "100.64.0.1",
            "0.0.0.0", "224.0.0.1", "255.255.255.255", "198.18.0.5", "::1", "fe80::1", "fc00::1", "fd12::1",
            "::", "ff02::1", "2001:db8::1",
        ] {
            assert!(!is_allowed_ip(ip(private), false), "{private} must be refused");
        }
    }

    #[test]
    fn addresses_embedded_in_ipv6_cannot_smuggle_a_private_ipv4() {
        assert!(!is_allowed_ip(ip("::ffff:127.0.0.1"), false), "IPv4-mapped loopback");
        assert!(!is_allowed_ip(ip("::ffff:10.0.0.1"), false), "IPv4-mapped private");
        assert!(!is_allowed_ip(ip("64:ff9b::7f00:1"), false), "NAT64 loopback");
        assert!(is_allowed_ip(ip("::ffff:8.8.8.8"), false), "IPv4-mapped public is fine");
    }

    #[test]
    fn lan_mode_opens_private_ranges_but_never_the_dangerous_ones() {
        for lan in ["127.0.0.1", "192.168.1.20", "10.0.0.2", "::1", "fd12::1", "::ffff:192.168.0.5"] {
            assert!(is_allowed_ip(ip(lan), true), "{lan} allowed on a local network");
        }
        for never in ["0.0.0.0", "224.0.0.1", "255.255.255.255", "169.254.169.254", "::", "ff02::1"] {
            assert!(!is_allowed_ip(ip(never), true), "{never} stays blocked");
        }
    }

    #[test]
    fn urls_are_checked_for_scheme_and_literal_hosts() {
        assert!(validate_url("https://example.com/list.m3u", false).is_ok());
        assert!(validate_url("  https://example.com/list.m3u  ", false).is_ok(), "surrounding spaces are fine");
        for bad in [
            "file:///etc/passwd",
            "ftp://example.com/x",
            "javascript:alert(1)",
            "http://127.0.0.1:8080/x",
            "http://[::1]/x",
            "http://169.254.169.254/latest/meta-data",
            "http://localhost/x",
            "http://printer.local/x",
            "not a url",
        ] {
            assert!(validate_url(bad, false).is_err(), "{bad} must be refused");
        }
        assert!(validate_url("http://192.168.1.5/list.m3u", true).is_ok(), "LAN mode");
        assert!(validate_url("http://localhost:8096/list.m3u", true).is_ok(), "LAN mode");
        assert!(validate_url("http://169.254.169.254/x", true).is_err(), "metadata stays blocked");
    }

    #[test]
    fn the_refusal_names_the_host_but_not_the_secret_in_the_url() {
        let error = validate_url("http://10.0.0.1/list.m3u?token=SECRET123", false).unwrap_err();
        let text = String::from(error);
        assert!(text.contains("10.0.0.1"));
        assert!(!text.contains("SECRET123"));
    }

    #[tokio::test]
    async fn a_name_that_resolves_to_loopback_is_refused_at_connect_time() {
        // `localhost` is caught by name in `check_url`; this exercises the resolver itself, which
        // is what stops a public-looking name that answers with a private address.
        let resolver = PublicOnlyResolver { allow_lan: false };
        let looked_up = resolver.resolve("localhost".parse().unwrap()).await;
        assert!(looked_up.is_err(), "localhost resolves only to loopback, so nothing is left");

        let lan = PublicOnlyResolver { allow_lan: true };
        let mut addrs = lan.resolve("localhost".parse().unwrap()).await.expect("allowed in LAN mode");
        assert!(addrs.next().is_some());
    }
}

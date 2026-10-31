//! What a URL handed to mpv is allowed to be.
//!
//! Playlist data is untrusted text from the internet. Before this module, an entry could point
//! mpv at a local file (`file:///…`), a LAN address, or a scheme Lumen never intends to open. The
//! checks here are deliberately small and pure so they can be tested without spawning a player.

use lumen_core::error::{PlayerError, Result};

/// Protocols mpv understands and Lumen deliberately supports. HTTP(S) is handled separately
/// because it also goes through `netguard`'s address checks.
const STREAMING_SCHEMES: [&str; 8] = [
    "rtsp", "rtsps", "rtmp", "rtmps", "mms", "mmsh", "udp", "rtp",
];

fn scheme_of(raw: &str) -> Option<String> {
    raw.split_once("://")
        .map(|(scheme, _)| scheme.trim().to_ascii_lowercase())
        .filter(|scheme| !scheme.is_empty())
}

fn is_local(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    lower.starts_with("file://") || raw.starts_with('/')
}

/// Rejects a playback URL that is outside the allowlist, points at a local file from library
/// data, or reaches a private address with local-network sources off.
///
/// `from_remote_playlist` is true when the URL came from a playlist that Lumen fetched (URL
/// import, extraction, harvest, catalog). A direct local file chosen by the user, or a local
/// file from a local playlist, is allowed because it never crossed a network boundary.
pub fn check_playback_url(raw: &str, from_remote_playlist: bool, allow_lan: bool) -> Result<()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(PlayerError::InvalidArg(
            "No stream address was given".to_string(),
        ));
    }

    if is_local(trimmed) {
        if from_remote_playlist {
            return Err(PlayerError::InvalidArg(
                "A playlist entry cannot open a local file".to_string(),
            ));
        }
        return Ok(());
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        // Literal private addresses and local hostnames are refused unless the user allowed
        // local-network sources; hostnames are checked again at connect time by netguard.
        lumen_core::netguard::validate_url(trimmed, allow_lan)?;
        return Ok(());
    }

    match scheme_of(trimmed).as_deref() {
        Some(scheme) if STREAMING_SCHEMES.contains(&scheme) => Ok(()),
        Some(scheme) => Err(PlayerError::InvalidArg(format!(
            "\"{scheme}://\" streams are not supported"
        ))),
        None => Err(PlayerError::InvalidArg(
            "That is not a stream address Lumen can open".to_string(),
        )),
    }
}

/// Network privacy is an HTTP(S) proxy. mpv's `--http-proxy` cannot carry rtsp/rtmp/udp/etc., so
/// those would connect directly and defeat the setting; refuse instead.
pub fn check_privacy_route(raw: &str) -> Result<()> {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || is_local(trimmed) {
        return Ok(());
    }
    Err(PlayerError::InvalidArg(format!(
        "Network privacy cannot protect \"{}://\" streams. Turn it off or use an HTTP(S) source.",
        scheme_of(trimmed).as_deref().unwrap_or("this")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_http_and_https_are_allowed() {
        assert!(check_playback_url("https://cdn.example/live.m3u8", true, false).is_ok());
        assert!(check_playback_url("http://example.com/live.m3u8", true, false).is_ok());
    }

    #[test]
    fn private_http_is_refused_unless_local_sources_are_allowed() {
        assert!(check_playback_url("http://192.168.1.10/live.m3u8", true, false).is_err());
        assert!(check_playback_url("http://192.168.1.10/live.m3u8", true, true).is_ok());
        assert!(check_playback_url("http://localhost/live.m3u8", true, false).is_err());
    }

    #[test]
    fn library_entries_cannot_open_local_files_but_direct_ones_can() {
        assert!(check_playback_url("file:///etc/passwd", true, false).is_err());
        assert!(check_playback_url("/home/user/movie.mkv", true, false).is_err());
        assert!(check_playback_url("file:///home/user/movie.mkv", false, false).is_ok());
        assert!(check_playback_url("/home/user/movie.mkv", false, false).is_ok());
    }

    #[test]
    fn unknown_schemes_are_refused() {
        assert!(check_playback_url("javascript:alert(1)", true, false).is_err());
        assert!(check_playback_url("data:text/html,hi", true, false).is_err());
        assert!(check_playback_url("smb://server/share", true, false).is_err());
    }

    #[test]
    fn mpv_streaming_protocols_are_allowed() {
        assert!(check_playback_url("rtsp://camera.local/live", true, true).is_ok());
        assert!(check_playback_url("rtmp://example.com/live", true, false).is_ok());
        assert!(check_playback_url("udp://239.0.0.1:1234", true, false).is_ok());
    }

    #[test]
    fn privacy_refuses_protocols_the_http_proxy_cannot_carry() {
        assert!(check_privacy_route("https://cdn.example/live.m3u8").is_ok());
        assert!(check_privacy_route("/home/user/movie.mkv").is_ok());
        assert!(check_privacy_route("rtsp://camera.local/live").is_err());
        assert!(check_privacy_route("rtmp://example.com/live").is_err());
        assert!(check_privacy_route("udp://239.0.0.1:1234").is_err());
    }
}

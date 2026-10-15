use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub id: Option<i64>,
    pub playlist_id: i64,
    pub name: String,
    pub url: String,
    pub group_title: Option<String>,
    pub tvg_id: Option<String>,
    pub tvg_logo: Option<String>,
    pub extra_headers: Option<String>, // JSON string
    pub last_ok: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub id: Option<i64>,
    pub name: String,
    pub source: String, // 'file' | 'url' | 'extract' | 'catalog'
    pub origin: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug)]
pub struct Extinf {
    #[allow(dead_code)]
    pub duration: i64,
    pub attrs: HashMap<String, String>,
    pub name: String,
}

/// The outcome of reading an M3U/M3U8 document.
#[derive(Debug)]
pub enum ParsedPlaylist {
    /// An IPTV channel list: `#EXTINF` entries, each followed by one stream URL.
    Channels(Vec<Channel>),
    /// An HLS master or media playlist. That is a single playable stream (its variants are
    /// renditions of it), never a list of channels.
    Hls(HlsManifest),
}

#[derive(Debug, Default)]
pub struct HlsManifest {
    /// Variant and rendition URIs of a master playlist, resolved against the playlist.
    pub variants: Vec<String>,
    /// Media segment URIs of a media playlist, resolved against the playlist.
    pub segments: Vec<String>,
}

impl HlsManifest {
    pub fn is_master(&self) -> bool {
        !self.variants.is_empty()
    }
}

/// `KEY="value"` pairs, shared by IPTV `#EXTINF` lines and HLS tags.
static ATTRIBUTE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"([A-Za-z0-9_-]+)="([^"]*)""#).expect("attribute regex is valid")
});

/// Attributes that only ever appear in IPTV channel lists, never in HLS playlists.
const IPTV_MARKERS: [&str; 4] = ["tvg-id", "tvg-name", "tvg-logo", "group-title"];

fn split_extinf_header_and_name(body: &str) -> (&str, &str) {
    let mut in_quotes = false;
    for (idx, ch) in body.char_indices() {
        if ch == '\"' {
            in_quotes = !in_quotes;
        } else if ch == ',' && !in_quotes {
            return (&body[..idx], &body[idx + 1..]);
        }
    }
    (body, "")
}

pub fn parse_extinf(line: &str) -> Option<Extinf> {
    let body = line.strip_prefix("#EXTINF:")?;
    let (attr_part_raw, name_raw) = split_extinf_header_and_name(body);
    let attr_part = attr_part_raw.trim();
    let name = name_raw.trim().to_string();

    let mut attrs = HashMap::new();
    let mut duration = -1;

    if let Some(first_token) = attr_part.split_whitespace().next() {
        let num_str = first_token.trim_end_matches(',');
        if let Ok(d) = num_str.parse::<i64>() {
            duration = d;
        } else if let Ok(df) = num_str.parse::<f64>() {
            duration = df as i64;
        }
    }

    for cap in ATTRIBUTE_RE.captures_iter(attr_part) {
        if let (Some(key), Some(value)) = (cap.get(1), cap.get(2)) {
            attrs.insert(key.as_str().to_lowercase(), value.as_str().to_string());
        }
    }

    Some(Extinf {
        duration,
        attrs,
        name,
    })
}

/// Read a single quoted attribute from a tag's attribute list, matched case-insensitively.
fn quoted_attribute(line: &str, key: &str) -> Option<String> {
    for cap in ATTRIBUTE_RE.captures_iter(line) {
        if cap
            .get(1)
            .is_some_and(|k| k.as_str().eq_ignore_ascii_case(key))
        {
            return cap.get(2).map(|v| v.as_str().to_string());
        }
    }
    None
}

fn is_hls_tag(line: &str) -> bool {
    line.trim_start()
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("#EXT-X-"))
}

fn has_iptv_markers(content: &str) -> bool {
    content.lines().any(|raw| {
        let line = raw.trim();
        if line.starts_with("#EXTGRP:") || line.starts_with("#EXTVLCOPT:") {
            return true;
        }
        match line.strip_prefix("#EXTINF:") {
            Some(rest) => IPTV_MARKERS.iter().any(|marker| rest.contains(marker)),
            None => false,
        }
    })
}

/// Both dialects start with `#EXTM3U` and use `#EXTINF`, so they are told apart by their
/// other tags: HLS playlists carry `#EXT-X-*` structural tags, IPTV channel lists carry
/// `tvg-*` / `group-title` attributes, `#EXTGRP` and `#EXTVLCOPT`. A channel list that
/// happens to include a stray `#EXT-X-` line still imports as channels.
pub fn is_hls_manifest(content: &str) -> bool {
    content.lines().any(is_hls_tag) && !has_iptv_markers(content)
}

/// Resolve a URI taken from a playlist against where that playlist came from.
///
/// Absolute URLs are returned untouched. Relative ones need a base — the URL of a remote
/// playlist, or the directory of a local file — because the stored string is handed to mpv
/// verbatim and mpv cannot resolve it for us. Returns `None` when the value is empty or
/// no usable base was supplied, so callers can keep the original text.
fn resolve_url(value: &str, base_url: Option<&str>) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    if Url::parse(value).is_ok() {
        return Some(value.to_string());
    }

    let joined = Url::parse(base_url?).ok()?.join(value).ok()?;

    Some(match joined.to_file_path() {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(_) => joined.to_string(),
    })
}

fn parse_hls_manifest(content: &str, base_url: Option<&str>) -> HlsManifest {
    let mut manifest = HlsManifest::default();
    let mut expects_variant = false;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with("#EXT-X-STREAM-INF") {
            expects_variant = true;
        } else if line.starts_with("#EXT-X-MEDIA:") || line.starts_with("#EXT-X-I-FRAME-STREAM-INF:")
        {
            // Alternative renditions (audio, subtitles, i-frames) belong to the same stream.
            if let Some(uri) = quoted_attribute(line, "URI") {
                manifest
                    .variants
                    .push(resolve_url(&uri, base_url).unwrap_or(uri));
            }
        } else if line.starts_with('#') {
            continue;
        } else if expects_variant {
            manifest
                .variants
                .push(resolve_url(line, base_url).unwrap_or_else(|| line.to_string()));
            expects_variant = false;
        } else {
            manifest
                .segments
                .push(resolve_url(line, base_url).unwrap_or_else(|| line.to_string()));
        }
    }

    manifest
}

pub fn parse_m3u(content: &str, playlist_id: i64, base_url: Option<&str>) -> Result<ParsedPlaylist> {
    if is_hls_manifest(content) {
        return Ok(ParsedPlaylist::Hls(parse_hls_manifest(content, base_url)));
    }

    let mut channels = Vec::new();
    let mut current_extinf: Option<Extinf> = None;
    let mut current_extgrp: Option<String> = None;
    let mut pending_headers: HashMap<String, String> = HashMap::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with("#EXTM3U") {
            continue;
        } else if line.starts_with("#EXTINF:") {
            current_extinf = parse_extinf(line);
        } else if let Some(grp) = line.strip_prefix("#EXTGRP:") {
            let grp_trimmed = grp.trim();
            if !grp_trimmed.is_empty() {
                current_extgrp = Some(grp_trimmed.to_string());
            }
        } else if let Some(opt) = line.strip_prefix("#EXTVLCOPT:") {
            let opt_trimmed = opt.trim();
            if let Some((k, v)) = opt_trimmed.split_once('=') {
                let key_lower = k.trim().to_lowercase();
                let val = v.trim().to_string();
                match key_lower.as_str() {
                    "http-user-agent" => {
                        pending_headers.insert("User-Agent".to_string(), val);
                    }
                    "http-referrer" | "http-referer" => {
                        pending_headers.insert("Referer".to_string(), val);
                    }
                    "http-origin" => {
                        pending_headers.insert("Origin".to_string(), val);
                    }
                    "http-cookie" => {
                        pending_headers.insert("Cookie".to_string(), val);
                    }
                    _ => {
                        pending_headers.insert(k.trim().to_string(), val);
                    }
                }
            }
        } else if line.starts_with('#') {
            continue;
        } else {
            // Stream URL line, resolved against the playlist it came from when relative.
            let url = resolve_url(line, base_url).unwrap_or_else(|| line.to_string());
            let (name, group_title, tvg_id, tvg_logo) = if let Some(ext) = current_extinf.take() {
                let resolved_name = if ext.name.is_empty() {
                    ext.attrs
                        .get("tvg-name")
                        .cloned()
                        .unwrap_or_else(|| url.clone())
                } else {
                    ext.name
                };
                let group = ext
                    .attrs
                    .get("group-title")
                    .cloned()
                    .or_else(|| current_extgrp.clone());
                let tid = ext.attrs.get("tvg-id").cloned();
                let tlogo = ext.attrs.get("tvg-logo").cloned();
                (resolved_name, group, tid, tlogo)
            } else {
                let fallback_name = url
                    .split('/')
                    .next_back()
                    .and_then(|s| s.split('?').next())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&url)
                    .to_string();
                (fallback_name, current_extgrp.clone(), None, None)
            };

            let extra_headers = if pending_headers.is_empty() {
                None
            } else {
                let json = serde_json::to_string(&pending_headers).ok();
                pending_headers.clear();
                json
            };

            channels.push(Channel {
                id: None,
                playlist_id,
                name,
                url,
                group_title,
                tvg_id,
                tvg_logo,
                extra_headers,
                last_ok: None,
                last_error: None,
            });
        }
    }

    Ok(ParsedPlaylist::Channels(channels))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channels_of(content: &str, base: Option<&str>) -> Vec<Channel> {
        match parse_m3u(content, 1, base).expect("parse should succeed") {
            ParsedPlaylist::Channels(channels) => channels,
            ParsedPlaylist::Hls(manifest) => panic!("expected channels, got HLS: {:?}", manifest),
        }
    }

    #[test]
    fn test_parse_m3u_with_commas_extgrp_and_vlcopt() {
        let sample = r#"#EXTM3U url-tvg="http://example.com/epg.xml"
#EXTINF:-1 tvg-id="bbc1.uk" tvg-logo="https://example.com/logo.png" group-title="News, UK",BBC One, HD
#EXTVLCOPT:http-user-agent=CustomAgent/1.0
#EXTVLCOPT:http-referrer=https://bbc.co.uk/
https://stream.example.com/live/bbc1.m3u8
#EXTINF:120,Simple Channel
#EXTGRP:Entertainment
https://stream.example.com/live/simple.m3u8
"#;

        let channels = channels_of(sample, None);
        assert_eq!(channels.len(), 2);

        assert_eq!(channels[0].name, "BBC One, HD");
        assert_eq!(channels[0].group_title.as_deref(), Some("News, UK"));
        assert_eq!(channels[0].tvg_id.as_deref(), Some("bbc1.uk"));
        assert_eq!(
            channels[0].tvg_logo.as_deref(),
            Some("https://example.com/logo.png")
        );
        let hdrs: HashMap<String, String> =
            serde_json::from_str(channels[0].extra_headers.as_deref().unwrap()).unwrap();
        assert_eq!(hdrs.get("User-Agent").map(String::as_str), Some("CustomAgent/1.0"));
        assert_eq!(hdrs.get("Referer").map(String::as_str), Some("https://bbc.co.uk/"));

        assert_eq!(channels[1].name, "Simple Channel");
        assert_eq!(channels[1].group_title.as_deref(), Some("Entertainment"));
        assert!(channels[1].extra_headers.is_none());
    }

    #[test]
    fn test_hls_media_manifest_is_not_a_channel_list() {
        let manifest = r#"#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:10
#EXT-X-MEDIA-SEQUENCE:0
#EXTINF:10.000,
seg-0.ts
#EXTINF:10.000,
seg-1.ts
#EXT-X-ENDLIST
"#;

        assert!(is_hls_manifest(manifest));

        match parse_m3u(manifest, 1, Some("https://cdn.example.com/live/index.m3u8")).unwrap() {
            ParsedPlaylist::Hls(parsed) => {
                assert!(!parsed.is_master());
                assert_eq!(
                    parsed.segments,
                    vec![
                        "https://cdn.example.com/live/seg-0.ts".to_string(),
                        "https://cdn.example.com/live/seg-1.ts".to_string()
                    ]
                );
                assert!(parsed.variants.is_empty());
            }
            ParsedPlaylist::Channels(channels) => {
                panic!("HLS manifest imported as {} channels", channels.len())
            }
        }
    }

    #[test]
    fn test_hls_master_manifest_variants_are_resolved() {
        let manifest = r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=640x360
low/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2560000,RESOLUTION=1280x720
high/index.m3u8
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="aud",NAME="English",URI="audio/eng.m3u8"
#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=100000,URI="iframe/index.m3u8"
"#;

        match parse_m3u(manifest, 1, Some("https://cdn.example.com/vod/title/master.m3u8")).unwrap() {
            ParsedPlaylist::Hls(parsed) => {
                assert!(parsed.is_master());
                assert_eq!(
                    parsed.variants,
                    vec![
                        "https://cdn.example.com/vod/title/low/index.m3u8".to_string(),
                        "https://cdn.example.com/vod/title/high/index.m3u8".to_string(),
                        "https://cdn.example.com/vod/title/audio/eng.m3u8".to_string(),
                        "https://cdn.example.com/vod/title/iframe/index.m3u8".to_string(),
                    ]
                );
                assert!(parsed.segments.is_empty());
            }
            ParsedPlaylist::Channels(channels) => {
                panic!("HLS master manifest imported as {} channels", channels.len())
            }
        }
    }

    #[test]
    fn test_relative_stream_urls_resolve_against_the_playlist() {
        let sample = r#"#EXTM3U
#EXTINF:-1 group-title="Local",Relative Path
streams/one.ts
#EXTINF:-1,Root Relative
/live/two.ts
#EXTINF:-1,Parent Relative
../shared/three.ts
#EXTINF:-1,Protocol Relative
//cdn.example.net/four.ts
#EXTINF:-1,Query Only
?channel=5
#EXTINF:-1,Absolute
https://other.example.com/five.m3u8
"#;

        let channels = channels_of(sample, Some("https://portal.example.com/lists/iptv.m3u"));
        let urls: Vec<&str> = channels.iter().map(|c| c.url.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "https://portal.example.com/lists/streams/one.ts",
                "https://portal.example.com/live/two.ts",
                "https://portal.example.com/shared/three.ts",
                "https://cdn.example.net/four.ts",
                "https://portal.example.com/lists/iptv.m3u?channel=5",
                "https://other.example.com/five.m3u8",
            ]
        );

        // Without a base the original text is preserved rather than guessed at.
        let unresolved = channels_of(sample, None);
        assert_eq!(unresolved[0].url, "streams/one.ts");
        assert_eq!(unresolved[5].url, "https://other.example.com/five.m3u8");
    }

    #[test]
    fn test_relative_stream_urls_resolve_against_a_local_directory() {
        let sample = r#"#EXTM3U
#EXTINF:-1,Local Clip
clips/one.mp4
#EXTINF:-1,Escape
../movies/two.mkv
"#;

        let channels = channels_of(sample, Some("file:///srv/media/playlists/"));
        assert_eq!(channels[0].url, "/srv/media/playlists/clips/one.mp4");
        assert_eq!(channels[1].url, "/srv/media/movies/two.mkv");
    }

    #[test]
    fn test_channel_list_with_stray_hls_tag_still_imports_as_channels() {
        let sample = r#"#EXTM3U
#EXT-X-ALLOW-CACHE:YES
#EXTINF:-1 tvg-id="six" group-title="News",Channel Six
https://stream.example.com/six.m3u8
"#;

        assert!(!is_hls_manifest(sample));
        let channels = channels_of(sample, None);
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].name, "Channel Six");
    }
}

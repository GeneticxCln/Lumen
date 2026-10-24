//! What the app was asked to open: files from "Open with Lumen", dropped files, links and
//! `lumen://` URLs. One place decides what each argument means, so launching, a second launch
//! forwarded to the running window, and drag-and-drop all behave the same.

use std::path::{Path, PathBuf};

use serde::Serialize;
use url::Url;

use crate::netguard;

/// The canonical list of extensions this app treats as a playable video, matching what
/// `tauri.conf.json`'s `fileAssociations` registers Lumen to open. Shared with `commands.rs`'s
/// Local Device folder browser and poster-generation gate, which used to keep their own,
/// shorter copies that quietly drifted out of sync with this one and with each other - a file
/// the OS would open with Lumen (and this module would then recognize) could be invisible when
/// just browsing its folder in the app.
pub const VIDEO_EXT: [&str; 11] = [
    "mp4", "m4v", "mkv", "avi", "mov", "webm", "ts", "m2ts", "mpg", "mpeg", "ogv",
];
pub const AUDIO_EXT: [&str; 7] = ["mp3", "flac", "wav", "ogg", "opus", "m4a", "aac"];
pub const PLAYLIST_EXT: [&str; 2] = ["m3u", "m3u8"];
const SUBTITLE_EXT: [&str; 5] = ["srt", "ass", "ssa", "vtt", "sub"];

/// More than this at once is not a person opening things; the rest is ignored.
pub const MAX_TARGETS: usize = 25;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum OpenTarget {
    /// A video or audio file on this computer.
    Media { path: String },
    /// An M3U/M3U8 playlist file, to add as a source.
    Playlist { path: String },
    /// A subtitle file, for what is playing.
    Subtitle { path: String },
    /// A web address to play. Anyone can send one (a web page can trigger a `lumen://` link), so the
    /// app asks before playing it.
    Stream { url: String },
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

fn classify_file(path: &Path) -> Option<OpenTarget> {
    let ext = extension(path);
    let path = path
        .canonicalize()
        .ok()
        .filter(|p| p.is_file())?
        .to_string_lossy()
        .into_owned();
    if VIDEO_EXT.contains(&ext.as_str()) || AUDIO_EXT.contains(&ext.as_str()) {
        Some(OpenTarget::Media { path })
    } else if PLAYLIST_EXT.contains(&ext.as_str()) {
        Some(OpenTarget::Playlist { path })
    } else if SUBTITLE_EXT.contains(&ext.as_str()) {
        Some(OpenTarget::Subtitle { path })
    } else {
        None
    }
}

/// Only public http(s) addresses are played. Local-network addresses need the "allow local
/// sources" setting, which a link must not be able to bypass, so they are refused here.
fn classify_stream(raw: &str) -> Option<OpenTarget> {
    let url = Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") || netguard::check_url(&url, false).is_err() {
        return None;
    }
    Some(OpenTarget::Stream {
        url: url.to_string(),
    })
}

fn classify_one(arg: &str, cwd: Option<&Path>) -> Option<OpenTarget> {
    let arg = arg.trim();
    if arg.is_empty() || arg.starts_with('-') {
        return None; // a command-line flag, not something to open
    }
    if let Some(rest) = arg.strip_prefix("lumen://") {
        // lumen://play?url=<address>
        let parsed = Url::parse(&format!("lumen://{rest}")).ok()?;
        if parsed.host_str() != Some("play") {
            return None;
        }
        let target = parsed
            .query_pairs()
            .find(|(key, _)| key == "url")?
            .1
            .into_owned();
        return classify_stream(&target);
    }
    if arg.starts_with("http://") || arg.starts_with("https://") {
        return classify_stream(arg);
    }
    if arg.starts_with("file://") {
        let path = Url::parse(arg).ok()?.to_file_path().ok()?;
        return classify_file(&path);
    }
    // Any other scheme (javascript:, data:, ftp:) is not a file path.
    if arg.contains("://") || arg.starts_with("javascript:") || arg.starts_with("data:") {
        return None;
    }
    let path = PathBuf::from(arg);
    let resolved = match (path.is_relative(), cwd) {
        (true, Some(cwd)) => cwd.join(path),
        _ => path,
    };
    classify_file(&resolved)
}

/// Turns launch arguments (without the program name) into things to open, dropping anything that
/// is not a real media, playlist or subtitle file or a public http(s) address.
pub fn classify_arguments(args: &[String], cwd: Option<&Path>) -> Vec<OpenTarget> {
    let mut targets: Vec<OpenTarget> = Vec::new();
    for arg in args {
        if let Some(target) = classify_one(arg, cwd)
            && !targets.contains(&target)
        {
            targets.push(target);
            if targets.len() >= MAX_TARGETS {
                break;
            }
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lumen-launch-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "Movie.MKV",
            "song.flac",
            "list.m3u8",
            "Movie.srt",
            "notes.txt",
        ] {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        std::fs::create_dir_all(dir.join("folder.mp4")).unwrap();
        dir
    }

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn files_are_sorted_into_media_playlists_and_subtitles() {
        let dir = fixture_dir();
        let path = |name: &str| {
            dir.join(name)
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        let given: Vec<String> = [
            "Movie.MKV",
            "song.flac",
            "list.m3u8",
            "Movie.srt",
            "notes.txt",
            "missing.mp4",
            "folder.mp4",
        ]
        .iter()
        .map(|n| dir.join(n).to_string_lossy().into_owned())
        .collect();
        assert_eq!(
            classify_arguments(&given, None),
            vec![
                OpenTarget::Media {
                    path: path("Movie.MKV")
                },
                OpenTarget::Media {
                    path: path("song.flac")
                },
                OpenTarget::Playlist {
                    path: path("list.m3u8")
                },
                OpenTarget::Subtitle {
                    path: path("Movie.srt")
                },
            ],
            "notes, a missing file and a folder named like a video are left out"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn relative_paths_resolve_against_the_launch_directory_and_file_urls_work() {
        let dir = fixture_dir();
        let expected = OpenTarget::Media {
            path: dir
                .join("Movie.MKV")
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        };
        assert_eq!(
            classify_arguments(&args(&["Movie.MKV"]), Some(&dir)),
            vec![expected.clone()]
        );
        let url = Url::from_file_path(dir.join("Movie.MKV"))
            .unwrap()
            .to_string();
        assert_eq!(classify_arguments(&[url], None), vec![expected]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn web_addresses_and_lumen_links_are_played_only_when_public_http() {
        assert_eq!(
            classify_arguments(&args(&["https://cdn.example.com/live/a.m3u8?x=1"]), None),
            vec![OpenTarget::Stream {
                url: "https://cdn.example.com/live/a.m3u8?x=1".into()
            }]
        );
        assert_eq!(
            classify_arguments(
                &args(&["lumen://play?url=https%3A%2F%2Fcdn.example.com%2Fa.m3u8"]),
                None
            ),
            vec![OpenTarget::Stream {
                url: "https://cdn.example.com/a.m3u8".into()
            }]
        );
        for refused in [
            "lumen://play?url=http%3A%2F%2F192.168.1.1%2Fadmin", // local network: not through a link
            "lumen://play?url=http%3A%2F%2F127.0.0.1%3A8080%2F",
            "lumen://play?url=file%3A%2F%2F%2Fetc%2Fpasswd",
            "lumen://play?url=javascript%3Aalert(1)",
            "lumen://play",                                 // no address
            "lumen://delete?url=https%3A%2F%2Fexample.com", // not an action we have
            "http://localhost:9000/x",
            "ftp://example.com/a.mp4",
            "javascript:alert(1)",
            "data:text/html,hi",
        ] {
            assert!(
                classify_arguments(&args(&[refused]), None).is_empty(),
                "{refused} must not be opened"
            );
        }
    }

    #[test]
    fn flags_are_ignored_duplicates_collapse_and_the_list_is_capped() {
        assert!(classify_arguments(&args(&["--flag", "-x", ""]), None).is_empty());
        let same = "https://cdn.example.com/a.m3u8";
        assert_eq!(classify_arguments(&args(&[same, same]), None).len(), 1);
        let many: Vec<String> = (0..100)
            .map(|i| format!("https://cdn.example.com/{i}.m3u8"))
            .collect();
        assert_eq!(classify_arguments(&many, None).len(), MAX_TARGETS);
    }
}

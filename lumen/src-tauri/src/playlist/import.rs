use crate::error::{PlayerError, Result};
pub use crate::playlist::m3u::{parse_m3u, Channel, ParsedPlaylist, Playlist};
use chrono::Utc;
use std::path::{Path, PathBuf};
use url::Url;

/// Directory a local playlist lives in, as a `file://` URL, so relative stream paths in it
/// resolve to the sibling files mpv should open.
fn file_base_url(path: &Path) -> Option<String> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => std::env::current_dir().ok()?,
    };
    let parent = std::fs::canonicalize(&parent).unwrap_or(parent);
    Url::from_directory_path(parent)
        .ok()
        .map(|url| url.to_string())
}

/// Turn parsed content into the channels of an import.
///
/// An HLS manifest is one stream — its variants are renditions of it — so it imports as a
/// single entry pointing at the manifest itself. mpv resolves master/media playlists and
/// their relative variant URIs natively; importing the manifest as channels would instead
/// create bogus "channels" out of its segment URIs.
fn parse_into_channels(
    content: &str,
    base_url: Option<&str>,
    origin: &str,
    entry_name: &str,
) -> Result<Vec<Channel>> {
    match parse_m3u(content, 0, base_url)? {
        ParsedPlaylist::Channels(channels) => Ok(channels),
        ParsedPlaylist::Hls(manifest) => {
            eprintln!(
                "lumen: {} is an HLS {} playlist ({} variant(s), {} segment(s)); importing it as a single playable entry",
                origin,
                if manifest.is_master() { "master" } else { "media" },
                manifest.variants.len(),
                manifest.segments.len(),
            );

            Ok(vec![Channel {
                id: None,
                playlist_id: 0,
                name: entry_name.to_string(),
                url: origin.to_string(),
                group_title: Some("HLS".to_string()),
                tvg_id: None,
                tvg_logo: None,
                extra_headers: None,
                last_ok: None,
                last_error: None,
            }])
        }
    }
}

pub async fn import_m3u_file(path: &str, name: Option<String>) -> Result<(Playlist, Vec<Channel>)> {
    let path_owned = path.to_string();
    let name_owned = name.clone();

    tokio::task::spawn_blocking(move || {
        let content = std::fs::read_to_string(&path_owned)
            .map_err(|e| PlayerError::Io(e.to_string()))?;
        let source_path = PathBuf::from(&path_owned);
        let playlist_name = name_owned.unwrap_or_else(|| {
            source_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Imported Playlist")
                .to_string()
        });

        let now = Utc::now().timestamp();
        let playlist = Playlist {
            id: None,
            name: playlist_name,
            source: "file".to_string(),
            origin: Some(path_owned.clone()),
            created_at: now,
            updated_at: now,
        };

        let channels = parse_into_channels(
            &content,
            file_base_url(&source_path).as_deref(),
            &path_owned,
            &playlist.name,
        )?;

        Ok((playlist, channels))
    })
    .await
    .map_err(|e| PlayerError::Parse(format!("Join error during M3U parse: {}", e)))?
}

pub async fn import_m3u_url(url: &str, name: Option<String>) -> Result<(Playlist, Vec<Channel>)> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) Lumen/0.1")
        .build()
        .map_err(|e| PlayerError::Http(e.to_string()))?;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| PlayerError::Http(e.to_string()))?;

    if !response.status().is_success() {
        return Err(PlayerError::Http(format!(
            "HTTP {} when fetching playlist",
            response.status()
        )));
    }

    let content = response
        .text()
        .await
        .map_err(|e| PlayerError::Http(e.to_string()))?;

    let url_owned = url.to_string();
    let name_owned = name.clone();

    tokio::task::spawn_blocking(move || {
        let playlist_name = name_owned.unwrap_or_else(|| {
            url_owned
                .split('/')
                .next_back()
                .and_then(|s| s.split('?').next())
                .filter(|s| !s.is_empty())
                .unwrap_or("Imported Playlist")
                .to_string()
        });

        let now = Utc::now().timestamp();
        let playlist = Playlist {
            id: None,
            name: playlist_name,
            source: "url".to_string(),
            origin: Some(url_owned.clone()),
            created_at: now,
            updated_at: now,
        };

        // Relative entries are resolved against the location of the playlist itself.
        let channels = parse_into_channels(&content, Some(&url_owned), &url_owned, &playlist.name)?;

        Ok((playlist, channels))
    })
    .await
    .map_err(|e| PlayerError::Parse(format!("Join error during M3U URL parse: {}", e)))?
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempPlaylist {
        dir: PathBuf,
    }

    impl TempPlaylist {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "lumen-import-{}-{}",
                label,
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::create_dir_all(dir.join("clips")).unwrap();
            // Canonicalize so expectations match the paths the resolver produces
            // (`env::temp_dir` can itself be a symlink on some platforms).
            let dir = std::fs::canonicalize(&dir).unwrap();
            Self { dir }
        }

        fn write(&self, name: &str, content: &str) -> String {
            let path = self.dir.join(name);
            std::fs::write(&path, content).unwrap();
            path.to_string_lossy().into_owned()
        }
    }

    impl Drop for TempPlaylist {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[tokio::test]
    async fn test_file_import_resolves_relative_stream_paths() {
        let fixture = TempPlaylist::new("relative");
        let path = fixture.write(
            "list.m3u",
            "#EXTM3U\n#EXTINF:-1 tvg-id=\"one\" group-title=\"Local\",Local Clip\nclips/one.mp4\n#EXTINF:-1,Remote\nhttps://example.com/two.m3u8\n",
        );

        let (playlist, channels) = import_m3u_file(&path, None).await.unwrap();

        assert_eq!(playlist.name, "list");
        assert_eq!(channels.len(), 2);
        assert_eq!(
            channels[0].url,
            fixture.dir.join("clips/one.mp4").to_string_lossy().to_string()
        );
        assert_eq!(channels[0].group_title.as_deref(), Some("Local"));
        assert_eq!(channels[1].url, "https://example.com/two.m3u8");
    }

    #[tokio::test]
    async fn test_hls_manifest_imports_as_one_playable_entry() {
        let fixture = TempPlaylist::new("hls");
        let path = fixture.write(
            "channel.m3u8",
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n#EXTINF:6.0,\nseg0.ts\n#EXTINF:6.0,\nseg1.ts\n#EXT-X-ENDLIST\n",
        );

        let (playlist, channels) = import_m3u_file(&path, None).await.unwrap();

        assert_eq!(channels.len(), 1, "segments must not become channels");
        assert_eq!(channels[0].url, path);
        assert_eq!(channels[0].name, "channel");
        assert_eq!(channels[0].group_title.as_deref(), Some("HLS"));
        assert!(channels[0].last_ok.is_none());
        assert_eq!(playlist.source, "file");
    }
}

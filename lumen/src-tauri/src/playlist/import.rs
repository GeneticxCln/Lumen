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
                language: None,
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

/// Largest playlist accepted from a URL. A response over this size is refused, not buffered,
/// so a hostile or runaway server cannot exhaust memory.
pub const MAX_PLAYLIST_BYTES: usize = 32 * 1024 * 1024;

/// Refuses a response whose declared `Content-Length` is already over the limit.
fn check_declared_length(declared: Option<u64>, limit: usize) -> Result<()> {
    match declared {
        Some(len) if len > limit as u64 => Err(PlayerError::Http(format!(
            "Playlist is {len} bytes, over the {limit} byte limit"
        ))),
        _ => Ok(()),
    }
}

/// Appends a chunk to the buffer, failing as soon as the total passes the limit.
fn append_capped(buffer: &mut Vec<u8>, chunk: &[u8], limit: usize) -> Result<()> {
    if buffer.len() + chunk.len() > limit {
        return Err(PlayerError::Http(format!(
            "Playlist is over the {limit} byte limit"
        )));
    }
    buffer.extend_from_slice(chunk);
    Ok(())
}

/// Reads the response body chunk by chunk and stops once it passes `limit`.
async fn read_capped_body(mut response: reqwest::Response, limit: usize) -> Result<String> {
    check_declared_length(response.content_length(), limit)?;
    let mut buffer = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| PlayerError::Http(crate::error::redacted(e)))?
    {
        append_capped(&mut buffer, &chunk, limit)?;
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

/// Fetches and parses a playlist. The address is checked before the request, at every redirect
/// and when the name resolves (see `netguard`); `allow_lan` is the user's opt-in for sources on
/// their own network.
pub async fn import_m3u_url(url: &str, name: Option<String>, allow_lan: bool) -> Result<(Playlist, Vec<Channel>)> {
    let target = crate::netguard::validate_url(url, allow_lan)?;

    let client = crate::netguard::guarded_client(allow_lan)
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) Lumen/0.1")
        .build()
        .map_err(|e| PlayerError::Http(e.to_string()))?;

    let response = client
        .get(target)
        .send()
        .await
        .map_err(|e| PlayerError::Http(crate::error::redacted(e)))?;

    if !response.status().is_success() {
        return Err(PlayerError::Http(format!(
            "HTTP {} when fetching playlist",
            response.status()
        )));
    }

    let content = read_capped_body(response, MAX_PLAYLIST_BYTES).await?;

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
mod body_limit_tests {
    use super::*;

    #[test]
    fn declared_length_over_limit_is_refused_before_reading() {
        assert!(check_declared_length(Some(101), 100).is_err());
        assert!(check_declared_length(Some(100), 100).is_ok());
        assert!(check_declared_length(None, 100).is_ok());
    }

    #[test]
    fn chunks_are_accepted_up_to_the_limit_and_no_further() {
        let mut buffer = Vec::new();
        append_capped(&mut buffer, &[1; 60], 100).unwrap();
        append_capped(&mut buffer, &[2; 40], 100).unwrap();
        assert_eq!(buffer.len(), 100);
        assert!(append_capped(&mut buffer, &[3], 100).is_err());
        assert_eq!(buffer.len(), 100, "a refused chunk must not be appended");
    }
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

#[cfg(test)]
mod network_guard_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A one-route HTTP server on loopback. `response` is the raw reply; `hits` counts requests.
    async fn serve(response: String) -> (u16, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buffer = [0u8; 2048];
                let _ = stream.read(&mut buffer).await;
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (port, hits)
    }

    fn ok_playlist() -> String {
        let body = "#EXTM3U\n#EXTINF:-1,One\nhttps://example.com/one.m3u8\n";
        format!("HTTP/1.1 200 OK\r\nContent-Type: audio/mpegurl\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    #[tokio::test]
    async fn a_local_address_is_refused_before_any_connection_is_made() {
        let (port, hits) = serve(ok_playlist()).await;
        let error = import_m3u_url(&format!("http://127.0.0.1:{port}/list.m3u"), None, false)
            .await
            .expect_err("loopback must be refused by default");
        assert!(String::from(error).contains("local"), "the message explains why");
        assert_eq!(hits.load(Ordering::SeqCst), 0, "the server was never contacted");
    }

    #[tokio::test]
    async fn local_network_sources_work_once_the_user_allows_them() {
        let (port, hits) = serve(ok_playlist()).await;
        let (_, channels) = import_m3u_url(&format!("http://127.0.0.1:{port}/list.m3u"), None, true)
            .await
            .expect("allowed in LAN mode");
        assert_eq!(channels.len(), 1);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_redirect_to_the_cloud_metadata_address_is_not_followed() {
        let redirect = "HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest/meta-data\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string();
        let (port, hits) = serve(redirect).await;
        // LAN mode lets the first hop (loopback) through, so only the redirect check can stop this.
        let error = import_m3u_url(&format!("http://127.0.0.1:{port}/list.m3u"), None, true)
            .await
            .expect_err("the redirect target is always blocked");
        assert_eq!(hits.load(Ordering::SeqCst), 1, "the first hop was reached");
        assert!(!String::from(error).is_empty());
    }
}

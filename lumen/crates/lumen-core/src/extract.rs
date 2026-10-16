//! Stream extraction via `yt-dlp`, plus managed binary installation with SHA-256 verification.
//!
//! Runs `yt-dlp -J --no-playlist -f "<prefers combined, else bestvideo+bestaudio merged>" <url>`
//! as a subprocess with a timeout and output size cap. Parses JSON (`url`, `http_headers`,
//! `title`, `requested_formats`). For split audio/video streams, both URLs are returned so mpv
//! can receive `--audio-file` instead of dropping audio.

use crate::error::{PlayerError, Result};
use crate::netguard;
use crate::playlist::import::read_capped_body;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Maximum JSON output accepted from `yt-dlp -J` (8 MiB).
pub const MAX_EXTRACT_JSON_BYTES: usize = 8 * 1024 * 1024;
/// Subprocess timeout for a single extraction.
pub const EXTRACT_TIMEOUT: Duration = Duration::from_secs(30);
/// Format selector: prefers a combined audio+video stream, else bestvideo+bestaudio merged.
pub const YTDLP_FORMAT_SPEC: &str = "best[vcodec!=none][acodec!=none]/bestvideo+bestaudio/best";

/// Pinned release manifest embedded at compile time.
pub const PINNED_MANIFEST_JSON: &str = include_str!("../yt-dlp-manifest.json");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtractResponse {
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_url: Option<String>,
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YtDlpAsset {
    pub asset: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YtDlpManifest {
    pub version: String,
    pub release_base: String,
    pub checksums_asset: String,
    pub assets: HashMap<String, YtDlpAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractorStatus {
    pub available: bool,
    pub source: String, // "managed" | "system" | "none"
    pub version: Option<String>,
    pub pinned_version: String,
    pub binary_path: Option<String>,
}

pub fn pinned_manifest() -> Result<YtDlpManifest> {
    serde_json::from_str(PINNED_MANIFEST_JSON)
        .map_err(|e| PlayerError::Parse(format!("Invalid yt-dlp manifest: {e}")))
}

pub fn current_target_triple() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64-unknown-linux-gnu"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}

/// Path where the managed yt-dlp binary lives inside the app data directory.
pub fn managed_binary_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("bin").join("yt-dlp")
}

/// Resolves which `yt-dlp` binary to run: managed binary in app data dir first, then system `yt-dlp` on PATH.
pub fn resolve_ytdlp_binary(app_data_dir: Option<&Path>) -> Option<(PathBuf, &'static str)> {
    if let Some(dir) = app_data_dir {
        let managed = managed_binary_path(dir);
        if managed.is_file() {
            return Some((managed, "managed"));
        }
    }
    if let Ok(output) = Command::new("yt-dlp")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        && output.status.success()
    {
        return Some((PathBuf::from("yt-dlp"), "system"));
    }
    None
}

/// Queries the version string of a yt-dlp binary, if runnable.
pub fn query_binary_version(bin: &Path) -> Option<String> {
    let output = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!version.is_empty()).then_some(version)
}

/// Reports the current extractor status (managed vs system PATH vs missing).
pub fn extractor_status(app_data_dir: Option<&Path>) -> Result<ExtractorStatus> {
    let manifest = pinned_manifest()?;
    match resolve_ytdlp_binary(app_data_dir) {
        Some((path, source)) => {
            let version = query_binary_version(&path);
            Ok(ExtractorStatus {
                available: true,
                source: source.to_string(),
                version,
                pinned_version: manifest.version,
                binary_path: Some(path.to_string_lossy().into_owned()),
            })
        }
        None => Ok(ExtractorStatus {
            available: false,
            source: "none".to_string(),
            version: None,
            pinned_version: manifest.version,
            binary_path: None,
        }),
    }
}

/// Computes the hex-encoded SHA-256 digest of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// Parses a `SHA2-256SUMS` file and returns the expected hex digest for `asset_name`.
pub fn parse_checksum_for_asset(checksums_text: &str, asset_name: &str) -> Option<String> {
    for line in checksums_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let hash = parts.next()?;
        let file = parts.next()?.trim_start_matches('*');
        if file == asset_name
            && hash.len() == 64
            && hash.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

/// Verifies downloaded binary bytes against the pinned manifest checksum (when given) and the
/// published `SHA2-256SUMS` content (when given).
pub fn verify_binary_checksum(
    bytes: &[u8],
    asset_name: &str,
    checksums_text: Option<&str>,
    expected_sha256: Option<&str>,
) -> Result<String> {
    let actual = sha256_hex(bytes);
    if let Some(sums) = checksums_text {
        let published = parse_checksum_for_asset(sums, asset_name).ok_or_else(|| {
            PlayerError::InvalidArg(format!(
                "Asset {asset_name} not found in published SHA2-256SUMS"
            ))
        })?;
        if actual != published.to_ascii_lowercase() {
            return Err(PlayerError::InvalidArg(format!(
                "SHA-256 mismatch against SHA2-256SUMS for {asset_name}: expected {published}, got {actual}"
            )));
        }
    }
    if let Some(expected) = expected_sha256 {
        let expected_lower = expected.trim().to_ascii_lowercase();
        if actual != expected_lower {
            return Err(PlayerError::InvalidArg(format!(
                "SHA-256 mismatch against pinned manifest for {asset_name}: expected {expected_lower}, got {actual}"
            )));
        }
    }
    Ok(actual)
}

/// Downloads and installs the managed `yt-dlp` binary into `app_data_dir/bin/yt-dlp`
/// after verifying its SHA-256 against the release's published `SHA2-256SUMS` and manifest.
pub async fn update_managed_extractor(
    app_data_dir: &Path,
    base_url_override: Option<&str>,
    allow_lan: bool,
) -> Result<ExtractorStatus> {
    let manifest = pinned_manifest()?;
    let triple = current_target_triple();
    let asset_info = manifest.assets.get(triple).ok_or_else(|| {
        PlayerError::InvalidArg(format!("No yt-dlp asset configured for target {triple}"))
    })?;

    let base = base_url_override
        .unwrap_or(&manifest.release_base)
        .trim_end_matches('/');
    let sums_url = format!("{base}/{}", manifest.checksums_asset);
    let bin_url = format!("{base}/{}", asset_info.asset);

    let sums_target = netguard::validate_url(&sums_url, allow_lan)?;
    let bin_target = netguard::validate_url(&bin_url, allow_lan)?;

    let client = netguard::guarded_client(allow_lan)
        .timeout(Duration::from_secs(60))
        .user_agent("Lumen/0.1")
        .build()
        .map_err(PlayerError::from)?;

    let sums_resp = client.get(sums_target).send().await.map_err(PlayerError::from)?;
    if !sums_resp.status().is_success() {
        return Err(PlayerError::Http(format!(
            "HTTP {} fetching yt-dlp checksums",
            sums_resp.status()
        )));
    }
    let sums_text = read_capped_body(sums_resp, 1024 * 1024).await?;

    let bin_resp = client.get(bin_target).send().await.map_err(PlayerError::from)?;
    if !bin_resp.status().is_success() {
        return Err(PlayerError::Http(format!(
            "HTTP {} fetching yt-dlp binary",
            bin_resp.status()
        )));
    }
    // Cap binary download at 64 MiB.
    const MAX_BINARY_BYTES: usize = 64 * 1024 * 1024;
    crate::playlist::import::check_declared_length(bin_resp.content_length(), MAX_BINARY_BYTES)?;
    let mut bin_bytes = Vec::new();
    let mut resp = bin_resp;
    while let Some(chunk) = resp.chunk().await.map_err(PlayerError::from)? {
        crate::playlist::import::append_capped(&mut bin_bytes, &chunk, MAX_BINARY_BYTES)?;
    }

    // The binary must match the hash pinned in this repository's manifest AND the release's
    // published SHA2-256SUMS. The pin is what makes the version "known good": the checksum file
    // comes from the same host as the binary, so on its own it would not stop a tampered release.
    let pinned = asset_info.sha256.trim().to_ascii_lowercase();
    if !is_sha256_hex(&pinned) {
        return Err(PlayerError::Parse(format!(
            "The pinned yt-dlp manifest has no valid SHA-256 for {}",
            asset_info.asset
        )));
    }
    verify_binary_checksum(&bin_bytes, &asset_info.asset, Some(&sums_text), Some(&pinned))?;

    let dest = managed_binary_path(app_data_dir);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = dest.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temp, &bin_bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&temp, &dest)?;

    extractor_status(Some(app_data_dir))
}

/// How long a resolved stream link is reused. Extracted links expire (hours, not days) and often
/// carry tokens, so they are never stored in the library; this only covers "extract, then play"
/// and quick replays, so the page is not resolved twice in a row.
pub const RESOLVE_CACHE_TTL: Duration = Duration::from_secs(10 * 60);
const RESOLVE_CACHE_MAX: usize = 32;

/// A small in-memory cache of resolved links keyed by page URL.
#[derive(Default)]
pub struct ResolveCache {
    entries: HashMap<String, (std::time::Instant, ExtractResponse)>,
}

impl ResolveCache {
    pub fn get(&self, page_url: &str, now: std::time::Instant) -> Option<ExtractResponse> {
        self.entries
            .get(page_url)
            .filter(|(stored, _)| now.saturating_duration_since(*stored) < RESOLVE_CACHE_TTL)
            .map(|(_, response)| response.clone())
    }

    pub fn put(&mut self, page_url: &str, response: ExtractResponse, now: std::time::Instant) {
        self.entries
            .retain(|_, (stored, _)| now.saturating_duration_since(*stored) < RESOLVE_CACHE_TTL);
        if self.entries.len() >= RESOLVE_CACHE_MAX
            && let Some(oldest) = self.entries.iter().min_by_key(|(_, (stored, _))| *stored).map(|(key, _)| key.clone())
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(page_url.to_string(), (now, response));
    }
}

static RESOLVED: LazyLock<std::sync::Mutex<ResolveCache>> = LazyLock::new(Default::default);

/// Remembers a freshly resolved link for the page it came from.
pub fn remember_resolved(page_url: &str, response: &ExtractResponse) {
    if let Ok(mut cache) = RESOLVED.lock() {
        cache.put(page_url, response.clone(), std::time::Instant::now());
    }
}

/// Resolves a page URL to a playable stream now, reusing a recent result when there is one.
pub async fn resolve_stream(
    page_url: &str,
    app_data_dir: Option<PathBuf>,
    allow_lan: bool,
) -> Result<ExtractResponse> {
    if let Ok(cache) = RESOLVED.lock()
        && let Some(hit) = cache.get(page_url, std::time::Instant::now())
    {
        return Ok(hit);
    }
    let resolved = extract_stream(page_url, app_data_dir, allow_lan).await?;
    remember_resolved(page_url, &resolved);
    Ok(resolved)
}

/// Runs a command with both a time limit and an output byte cap on stdout.
pub fn run_capped_with_timeout(
    mut command: Command,
    limit: Duration,
    max_stdout_bytes: usize,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    let program = command.get_program().to_string_lossy().into_owned();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);

    let mut child = command
        .spawn()
        .map_err(|e| PlayerError::Io(format!("Could not start {program}: {e}")))?;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let stdout_handle = thread::spawn(move || -> std::result::Result<Vec<u8>, String> {
        let mut buf = Vec::new();
        if let Some(mut pipe) = stdout_pipe {
            let mut chunk = [0u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        if buf.len() + n > max_stdout_bytes {
                            return Err(format!(
                                "Output exceeded the {max_stdout_bytes} byte limit"
                            ));
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.to_string()),
                }
            }
        }
        Ok(buf)
    });

    let stderr_handle = thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::new();
        if let Some(pipe) = stderr_pipe {
            let _ = pipe.take(256 * 1024).read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                #[cfg(unix)]
                let _ = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(child.id() as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
                let _ = child.kill();
                let _ = child.wait();
                drop((stdout_handle, stderr_handle));
                return Err(PlayerError::Io(format!(
                    "{program} did not finish within {} seconds and was stopped",
                    limit.as_secs()
                )));
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    };

    let stdout = match stdout_handle.join() {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(reason)) => return Err(PlayerError::Parse(reason)),
        Err(_) => return Err(PlayerError::Io("Failed to read extractor output".into())),
    };
    let stderr = stderr_handle.join().unwrap_or_default();

    Ok((status, stdout, stderr))
}

fn clean_headers(raw: Option<&serde_json::Value>) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    let Some(obj) = raw.and_then(|v| v.as_object()) else {
        return headers;
    };
    for (k, v) in obj.iter().take(32) {
        if let Some(val) = v.as_str()
            && !k.trim().is_empty()
            && !val.chars().any(char::is_control)
        {
            headers.insert(k.clone(), val.to_string());
        }
    }
    headers
}

fn has_video(fmt: &serde_json::Value) -> bool {
    match fmt.get("vcodec").and_then(|v| v.as_str()) {
        Some(codec) => codec != "none" && !codec.is_empty(),
        None => false,
    }
}

fn has_audio(fmt: &serde_json::Value) -> bool {
    match fmt.get("acodec").and_then(|v| v.as_str()) {
        Some(codec) => codec != "none" && !codec.is_empty(),
        None => false,
    }
}

/// Parses `yt-dlp -J` JSON output into an `ExtractResponse`.
///
/// Handles:
/// - Single video or playlist/multi-video (picks first valid entry).
/// - Combined format (`url` at top level).
/// - Split video + audio (`requested_formats` with two entries, returning both `url` and `audio_url`
///   so mpv can attach `--audio-file`).
/// - Fallback to last format in `formats` with a playable URL.
pub fn parse_ytdlp_json(raw_json: &[u8], fallback_title: &str, allow_lan: bool) -> Result<ExtractResponse> {
    if raw_json.len() > MAX_EXTRACT_JSON_BYTES {
        return Err(PlayerError::Parse(format!(
            "Extractor output exceeds {MAX_EXTRACT_JSON_BYTES} bytes"
        )));
    }
    let root: serde_json::Value = serde_json::from_slice(raw_json)
        .map_err(|e| PlayerError::Parse(format!("Invalid extractor JSON: {e}")))?;

    let info = if let Some(entries) = root.get("entries").and_then(|e| e.as_array()) {
        entries
            .iter()
            .find(|e| !e.is_null() && e.is_object())
            .ok_or_else(|| PlayerError::NotFound("Playlist had no playable entries".into()))?
    } else {
        &root
    };

    let title = info
        .get("title")
        .and_then(|t| t.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(fallback_title)
        .to_string();

    let mut headers = clean_headers(info.get("http_headers"));
    let mut video_url: Option<String> = None;
    let mut audio_url: Option<String> = None;

    // Check `requested_formats` first for split video+audio streams.
    if let Some(req_formats) = info.get("requested_formats").and_then(|f| f.as_array())
        && !req_formats.is_empty()
    {
        for fmt in req_formats {
            let Some(u) = fmt.get("url").and_then(|u| u.as_str()).map(str::trim).filter(|u| !u.is_empty()) else {
                continue;
            };
            if headers.is_empty() {
                headers = clean_headers(fmt.get("http_headers"));
            }
            if has_video(fmt) && !has_audio(fmt) && video_url.is_none() {
                video_url = Some(u.to_string());
            } else if has_audio(fmt) && !has_video(fmt) && audio_url.is_none() {
                audio_url = Some(u.to_string());
            } else if video_url.is_none() {
                video_url = Some(u.to_string());
            } else if audio_url.is_none() {
                audio_url = Some(u.to_string());
            }
        }
    }

    // Direct `url` field when not split.
    if video_url.is_none()
        && let Some(u) = info.get("url").and_then(|u| u.as_str()).map(str::trim).filter(|u| !u.is_empty())
    {
        video_url = Some(u.to_string());
    }

    // Fallback to `formats` list.
    if video_url.is_none()
        && let Some(formats) = info.get("formats").and_then(|f| f.as_array())
    {
        if let Some(last) = formats
            .iter()
            .rev()
            .find(|f| f.get("url").and_then(|u| u.as_str()).is_some_and(|s| !s.trim().is_empty()))
        {
            video_url = last.get("url").and_then(|u| u.as_str()).map(|s| s.trim().to_string());
            if headers.is_empty() {
                headers = clean_headers(last.get("http_headers"));
            }
        }
    }

    let primary_url = video_url.ok_or_else(|| {
        PlayerError::NotFound("Could not resolve a direct playable stream URL".into())
    })?;

    netguard::validate_url(&primary_url, allow_lan)?;
    if let Some(ref aud) = audio_url {
        netguard::validate_url(aud, allow_lan)?;
    }

    Ok(ExtractResponse {
        title,
        url: primary_url,
        audio_url,
        headers,
    })
}

/// Runs `yt-dlp` on `url` and returns the extracted stream URL(s), title, and HTTP headers.
pub async fn extract_stream(
    url: &str,
    app_data_dir: Option<PathBuf>,
    allow_lan: bool,
) -> Result<ExtractResponse> {
    let validated = netguard::validate_url(url, allow_lan)?;
    let url_str = validated.to_string();

    let (binary, _source) = resolve_ytdlp_binary(app_data_dir.as_deref()).ok_or_else(|| {
        PlayerError::NotFound(
            "yt-dlp is not installed. Use 'Update extractor' in Settings or install yt-dlp on your system.".into(),
        )
    })?;

    tokio::task::spawn_blocking(move || {
        let mut cmd = Command::new(&binary);
        cmd.args([
            "-J",
            "--no-playlist",
            "--no-warnings",
            "--socket-timeout",
            "15",
            "-f",
            YTDLP_FORMAT_SPEC,
            "--",
            &url_str,
        ]);

        let (status, stdout, stderr) =
            run_capped_with_timeout(cmd, EXTRACT_TIMEOUT, MAX_EXTRACT_JSON_BYTES)?;

        if !status.success() {
            let err_text = String::from_utf8_lossy(&stderr).trim().to_string();
            let detail = err_text
                .lines()
                .last()
                .filter(|l| !l.is_empty())
                .unwrap_or("yt-dlp failed to extract a stream");
            return Err(PlayerError::Io(detail.to_string()));
        }

        parse_ytdlp_json(&stdout, &url_str, allow_lan)
    })
    .await
    .map_err(|e| PlayerError::Io(format!("Extractor task join error: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_combined_stream_json() {
        let raw = br#"{
            "title": "Big Buck Bunny",
            "url": "https://cdn.example.com/bbb.mp4",
            "http_headers": {
                "User-Agent": "CustomUA/1.0",
                "Referer": "https://example.com/"
            }
        }"#;
        let res = parse_ytdlp_json(raw, "https://example.com/watch", false).unwrap();
        assert_eq!(res.title, "Big Buck Bunny");
        assert_eq!(res.url, "https://cdn.example.com/bbb.mp4");
        assert_eq!(res.audio_url, None);
        assert_eq!(res.headers.get("User-Agent").map(String::as_str), Some("CustomUA/1.0"));
        assert_eq!(res.headers.get("Referer").map(String::as_str), Some("https://example.com/"));
    }

    #[test]
    fn parses_split_video_and_audio_requested_formats() {
        let raw = br#"{
            "title": "Split Stream 4K",
            "requested_formats": [
                {
                    "url": "https://cdn.example.com/video-2160p.mp4",
                    "vcodec": "vp9",
                    "acodec": "none",
                    "http_headers": {"User-Agent": "TestUA"}
                },
                {
                    "url": "https://cdn.example.com/audio-opus.webm",
                    "vcodec": "none",
                    "acodec": "opus"
                }
            ]
        }"#;
        let res = parse_ytdlp_json(raw, "https://example.com/watch", false).unwrap();
        assert_eq!(res.title, "Split Stream 4K");
        assert_eq!(res.url, "https://cdn.example.com/video-2160p.mp4");
        assert_eq!(
            res.audio_url.as_deref(),
            Some("https://cdn.example.com/audio-opus.webm")
        );
        assert_eq!(res.headers.get("User-Agent").map(String::as_str), Some("TestUA"));
    }

    #[test]
    fn rejects_private_stream_urls_from_extractor_unless_lan_enabled() {
        let raw = br#"{
            "title": "Loopback trap",
            "url": "http://127.0.0.1:8080/secret.m3u8"
        }"#;
        assert!(parse_ytdlp_json(raw, "https://example.com", false).is_err());
        assert!(parse_ytdlp_json(raw, "https://example.com", true).is_ok());
    }

    #[test]
    fn verifies_sha256_against_checksums_file_and_manifest() {
        let payload = b"fake-yt-dlp-binary-contents";
        let digest = sha256_hex(payload);
        let sums = format!(
            "{digest}  yt-dlp_linux\n0000000000000000000000000000000000000000000000000000000000000000  other_file\n"
        );
        assert_eq!(
            verify_binary_checksum(payload, "yt-dlp_linux", Some(&sums), Some(&digest)).unwrap(),
            digest
        );
        // Tampered payload fails verification
        assert!(verify_binary_checksum(b"tampered", "yt-dlp_linux", Some(&sums), Some(&digest)).is_err());
        // Missing asset in SHA2-256SUMS fails verification
        assert!(verify_binary_checksum(payload, "missing_asset", Some(&sums), None).is_err());
    }

    /// The two lines of the real upstream `SHA2-256SUMS` for the pinned release (downloaded from
    /// the release page and the binary itself hashed). The manifest once shipped with different
    /// values, and nothing noticed because the pin was not enforced.
    const UPSTREAM_SUMS_2026_02_04: &str = "\
051ed827a836f56772d662c38ebfabd8d613a21a39376737d56893184bcc53f8  yt-dlp_linux
6b83f090768a9d428456b7998a5b8fc1c628895647df2763f54f9552601b732e  yt-dlp_linux_aarch64
";

    #[test]
    fn the_pinned_manifest_matches_the_upstream_release_checksums() {
        let manifest = pinned_manifest().unwrap();
        assert_eq!(manifest.version, "2026.02.04", "update UPSTREAM_SUMS_2026_02_04 when the pin moves");
        assert!(!manifest.assets.is_empty());
        for (target, asset) in &manifest.assets {
            let published = parse_checksum_for_asset(UPSTREAM_SUMS_2026_02_04, &asset.asset)
                .unwrap_or_else(|| panic!("no upstream checksum recorded for {target} ({})", asset.asset));
            assert_eq!(asset.sha256, published, "pinned hash for {target}");
            assert!(is_sha256_hex(&asset.sha256));
        }
    }

    #[test]
    fn an_empty_pin_cannot_disable_verification() {
        let payload = b"binary";
        assert!(verify_binary_checksum(payload, "yt-dlp_linux", None, Some("")).is_err());
        assert!(!is_sha256_hex(""));
    }

    /// Downloads the pinned yt-dlp from GitHub, verifies it against the manifest and the release
    /// checksums, installs it and runs it. Needs network: `cargo test -p lumen-core live_ -- --ignored`.
    #[tokio::test]
    #[ignore = "downloads about 36 MB from GitHub"]
    async fn live_update_installs_and_runs_the_pinned_extractor() {
        let dir = std::env::temp_dir().join(format!("lumen-ytdlp-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let status = update_managed_extractor(&dir, None, false).await.expect("pinned download verifies");
        assert!(status.available);
        assert_eq!(status.source, "managed");
        assert_eq!(status.version.as_deref(), Some("2026.02.04"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolved_links_are_reused_briefly_and_then_dropped() {
        let mut cache = ResolveCache::default();
        let start = std::time::Instant::now();
        let response = ExtractResponse { title: "T".into(), url: "https://cdn.example/v.mp4?sig=abc".into(), audio_url: None, headers: HashMap::new() };
        assert!(cache.get("https://page.example/watch", start).is_none());
        cache.put("https://page.example/watch", response.clone(), start);
        assert_eq!(cache.get("https://page.example/watch", start + Duration::from_secs(60)), Some(response));
        assert!(cache.get("https://page.example/watch", start + RESOLVE_CACHE_TTL).is_none(), "expired links are not served");
        assert!(cache.get("https://other.example/", start).is_none());
    }

    #[test]
    fn the_resolve_cache_is_bounded() {
        let mut cache = ResolveCache::default();
        let start = std::time::Instant::now();
        let response = ExtractResponse { title: "T".into(), url: "https://cdn.example/v".into(), audio_url: None, headers: HashMap::new() };
        for index in 0..(RESOLVE_CACHE_MAX + 10) {
            cache.put(&format!("https://page.example/{index}"), response.clone(), start + Duration::from_millis(index as u64));
        }
        assert_eq!(cache.entries.len(), RESOLVE_CACHE_MAX);
        assert!(cache.get("https://page.example/0", start + Duration::from_secs(1)).is_none(), "the oldest was evicted");
    }
}

//! A small web server for the files on this computer that are sent to a TV: the TV fetches the
//! file from here. Only files the user chose are served, under a random address, and only to the
//! one device they were sent to.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::error::{PlayerError, Result};

const MAX_HEAD_BYTES: usize = 16 * 1024;
const CHUNK: usize = 64 * 1024;

/// The media type a TV is told for a file, from its extension.
pub fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mp4" | "m4v") => "video/mp4",
        Some("mkv") => "video/x-matroska",
        Some("webm") => "video/webm",
        Some("avi") => "video/x-msvideo",
        Some("mov") => "video/quicktime",
        Some("ts" | "m2ts" | "mts") => "video/mp2t",
        Some("mpg" | "mpeg") => "video/mpeg",
        Some("mp3") => "audio/mpeg",
        Some("flac") => "audio/flac",
        Some("m4a" | "aac") => "audio/mp4",
        Some("ogg" | "oga") => "audio/ogg",
        Some("wav") => "audio/wav",
        Some("srt") => "application/x-subrip",
        _ => "application/octet-stream",
    }
}

/// The local address that reaches `peer`; the address a TV can use to call back. Nothing is sent.
pub fn local_address_for(peer: IpAddr) -> Result<IpAddr> {
    let bind: SocketAddr = if peer.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    let socket = std::net::UdpSocket::bind(bind)?;
    socket.connect((peer, 9))?;
    Ok(socket.local_addr()?.ip())
}

type Files = Arc<Mutex<HashMap<String, PathBuf>>>;

/// A running server. Dropping it stops it.
pub struct MediaServer {
    address: SocketAddr,
    files: Files,
    task: tokio::task::JoinHandle<()>,
}

impl MediaServer {
    /// Starts listening on `bind` (a local address, port chosen by the system) and answers only
    /// connections from `peer`.
    pub async fn start(bind: IpAddr, peer: IpAddr) -> Result<Self> {
        let listener = TcpListener::bind((bind, 0)).await?;
        let address = listener.local_addr()?;
        let files: Files = Arc::default();
        let shared = files.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, from)) = listener.accept().await else {
                    break;
                };
                if from.ip() != peer {
                    continue; // Dropped: not the device this was started for.
                }
                let files = shared.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, files).await;
                });
            }
        });
        Ok(Self {
            address,
            files,
            task,
        })
    }

    /// Makes `path` available and returns the address to give the TV.
    pub fn add(&self, path: &Path) -> Result<String> {
        let metadata = std::fs::metadata(path)?;
        if !metadata.is_file() {
            return Err(PlayerError::InvalidArg("That is not a file".into()));
        }
        let mut raw = [0u8; 16];
        getrandom::fill(&mut raw).map_err(|e| PlayerError::Io(e.to_string()))?;
        let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let name = path
            .extension()
            .and_then(|e| e.to_str())
            .filter(|e| e.chars().all(|c| c.is_ascii_alphanumeric()))
            .map(|e| format!("media.{e}"))
            .unwrap_or_else(|| "media".into());
        self.files
            .lock()
            .unwrap()
            .insert(token.clone(), path.to_path_buf());
        Ok(format!("http://{}/m/{token}/{name}", self.address))
    }
}

impl Drop for MediaServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn read_head(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while head.len() < MAX_HEAD_BYTES {
        if stream.read(&mut byte).await? == 0 {
            break;
        }
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            return Ok(String::from_utf8_lossy(&head).into_owned());
        }
    }
    Err(std::io::ErrorKind::InvalidData.into())
}

/// A requested byte range, resolved against the file size. `None` means the range cannot be met.
pub fn parse_range(header: &str, size: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') || size == 0 {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    let (start, end) = match (start.trim(), end.trim()) {
        ("", suffix) => {
            let n: u64 = suffix.parse().ok()?;
            (size.saturating_sub(n.min(size)), size - 1)
        }
        (start, "") => (start.parse().ok()?, size - 1),
        (start, end) => (start.parse().ok()?, end.parse::<u64>().ok()?.min(size - 1)),
    };
    (start <= end && start < size).then_some((start, end))
}

async fn reply(stream: &mut TcpStream, status: &str, headers: &str) -> std::io::Result<()> {
    stream
        .write_all(format!("HTTP/1.1 {status}\r\n{headers}Connection: close\r\n\r\n").as_bytes())
        .await
}

async fn serve(mut stream: TcpStream, files: Files) -> std::io::Result<()> {
    let head = tokio::time::timeout(std::time::Duration::from_secs(10), read_head(&mut stream))
        .await
        .map_err(|_| std::io::ErrorKind::TimedOut)??;
    let mut lines = head.split("\r\n");
    let request = lines.next().unwrap_or_default();
    let mut parts = request.split(' ');
    let (method, target) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    let mut range = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("range")
        {
            range = Some(value.to_string());
        }
    }
    if method != "GET" && method != "HEAD" {
        return reply(
            &mut stream,
            "405 Method Not Allowed",
            "Allow: GET, HEAD\r\nContent-Length: 0\r\n",
        )
        .await;
    }
    // /m/<token>/<name>
    let token = target
        .strip_prefix("/m/")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_default();
    let path = files.lock().unwrap().get(token).cloned();
    let Some(path) = path else {
        return reply(&mut stream, "404 Not Found", "Content-Length: 0\r\n").await;
    };
    let Ok(mut file) = tokio::fs::File::open(&path).await else {
        return reply(&mut stream, "404 Not Found", "Content-Length: 0\r\n").await;
    };
    let size = file.metadata().await?.len();
    let (status, start, end) = match range.as_deref() {
        None => ("200 OK", 0, size.saturating_sub(1)),
        Some(header) => match parse_range(header, size) {
            Some((start, end)) => ("206 Partial Content", start, end),
            None => {
                return reply(
                    &mut stream,
                    "416 Range Not Satisfiable",
                    &format!("Content-Range: bytes */{size}\r\nContent-Length: 0\r\n"),
                )
                .await;
            }
        },
    };
    let length = if size == 0 { 0 } else { end - start + 1 };
    let mut headers = format!(
        "Content-Type: {}\r\nContent-Length: {length}\r\nAccept-Ranges: bytes\r\ntransferMode.dlna.org: Streaming\r\ncontentFeatures.dlna.org: DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000\r\n",
        mime_for(&path)
    );
    if status.starts_with("206") {
        headers.push_str(&format!("Content-Range: bytes {start}-{end}/{size}\r\n"));
    }
    reply(&mut stream, status, &headers).await?;
    if method == "HEAD" || length == 0 {
        return Ok(());
    }
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let mut remaining = length;
    let mut buffer = vec![0u8; CHUNK];
    while remaining > 0 {
        let want = remaining.min(CHUNK as u64) as usize;
        let read = file.read(&mut buffer[..want]).await?;
        if read == 0 {
            break;
        }
        stream.write_all(&buffer[..read]).await?;
        remaining -= read as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_are_resolved_against_the_file_size() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=900-", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(
            parse_range("bytes=500-5000", 1000),
            Some((500, 999)),
            "end is clamped"
        );
        assert_eq!(parse_range("bytes=1000-", 1000), None);
        assert_eq!(parse_range("bytes=9-3", 1000), None);
        assert_eq!(parse_range("bytes=0-1,5-6", 1000), None, "one range only");
        assert_eq!(parse_range("items=0-1", 1000), None);
        assert_eq!(parse_range("bytes=0-0", 0), None);
    }

    #[test]
    fn the_type_follows_the_extension() {
        assert_eq!(mime_for(Path::new("a/Movie.MKV")), "video/x-matroska");
        assert_eq!(mime_for(Path::new("a.mp4")), "video/mp4");
        assert_eq!(mime_for(Path::new("a.unknown")), "application/octet-stream");
    }
}

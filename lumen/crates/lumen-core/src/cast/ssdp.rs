//! Finding UPnP media renderers (smart TVs, game consoles, receivers) on the local network with
//! SSDP: one multicast search, then whoever answers describes itself with a `LOCATION` address.

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::net::UdpSocket;
use url::Url;

use crate::error::Result;

/// The multicast group and port every UPnP device listens on.
pub const MULTICAST: SocketAddr = SocketAddr::new(
    std::net::IpAddr::V4(Ipv4Addr::new(239, 255, 255, 250)),
    1900,
);

/// What a TV that can be sent a video answers to.
pub const MEDIA_RENDERER: &str = "urn:schemas-upnp-org:device:MediaRenderer:1";

/// One answer to a search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub location: Url,
    pub usn: String,
}

/// The search message. `mx` is how many seconds a device may spread its answer over.
pub fn search_request(target: &str, mx: u8) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: {mx}\r\nST: {target}\r\nUSER-AGENT: Lumen/1 UPnP/1.1\r\n\r\n"
    )
}

/// Reads one search answer. Anything that is not a `200 OK` with a usable `LOCATION` is dropped,
/// as are `NOTIFY` messages other devices multicast on their own.
pub fn parse_reply(bytes: &[u8]) -> Option<Reply> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.split("\r\n");
    let status = lines.next()?;
    if !status.starts_with("HTTP/1.1 200") && !status.starts_with("HTTP/1.0 200") {
        return None;
    }
    let mut location = None;
    let mut usn = String::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "location" => location = Url::parse(value.trim()).ok(),
            "usn" => usn = value.trim().to_string(),
            _ => {}
        }
    }
    let location = location.filter(|url| url.scheme() == "http")?;
    Some(Reply { location, usn })
}

/// Sends the search to `target` (the multicast group, or a test responder) and collects the
/// distinct answers that arrive within `wait`.
pub async fn search(target: SocketAddr, search_target: &str, wait: Duration) -> Result<Vec<Reply>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    let request = search_request(search_target, wait.as_secs().clamp(1, 5) as u8);
    // UDP loses packets; asking twice is what other clients do.
    for _ in 0..2 {
        socket.send_to(request.as_bytes(), target).await?;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut seen = HashSet::new();
    let mut replies = Vec::new();
    let deadline = tokio::time::Instant::now() + wait;
    let mut buffer = vec![0u8; 4096];
    while let Ok(Ok((length, _))) =
        tokio::time::timeout_at(deadline, socket.recv_from(&mut buffer)).await
    {
        if let Some(reply) = parse_reply(&buffer[..length])
            && seen.insert(reply.location.clone())
        {
            replies.push(reply);
        }
    }
    Ok(replies)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_search_asks_for_media_renderers() {
        let request = search_request(MEDIA_RENDERER, 2);
        assert!(request.starts_with("M-SEARCH * HTTP/1.1\r\n"));
        assert!(request.contains("MAN: \"ssdp:discover\""));
        assert!(request.contains(&format!("ST: {MEDIA_RENDERER}")));
        assert!(request.ends_with("\r\n\r\n"));
    }

    #[test]
    fn an_answer_gives_the_address_of_the_description() {
        let answer = b"HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nlocation: http://192.168.1.50:49152/desc.xml\r\nST: urn:schemas-upnp-org:device:MediaRenderer:1\r\nUSN: uuid:abc::urn:schemas-upnp-org:device:MediaRenderer:1\r\n\r\n";
        let reply = parse_reply(answer).unwrap();
        assert_eq!(
            reply.location.as_str(),
            "http://192.168.1.50:49152/desc.xml"
        );
        assert!(reply.usn.starts_with("uuid:abc"));
    }

    #[test]
    fn other_traffic_is_not_an_answer() {
        assert!(
            parse_reply(b"NOTIFY * HTTP/1.1\r\nLOCATION: http://10.0.0.2/d.xml\r\n\r\n").is_none()
        );
        assert!(
            parse_reply(b"HTTP/1.1 200 OK\r\nST: x\r\n\r\n").is_none(),
            "no location"
        );
        assert!(parse_reply(b"HTTP/1.1 200 OK\r\nLOCATION: file:///etc/passwd\r\n\r\n").is_none());
        assert!(
            parse_reply(b"HTTP/1.1 200 OK\r\nLOCATION: https://10.0.0.2/d.xml\r\n\r\n").is_none()
        );
        assert!(parse_reply(&[0xff, 0xfe, 0x00]).is_none());
    }
}

//! Clients for the two upstream proxy protocols the relay can forward to: SOCKS5 (RFC 1928 +
//! RFC 1929 username/password auth) and plain HTTP (`CONNECT` for tunnels, absolute-form
//! forwarding for plain `http://` requests). This is the only code in the app that speaks either
//! protocol; everything else just talks to the local relay.

use std::net::IpAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::{PlayerError, Result};

fn upstream_error(reason: impl std::fmt::Display) -> PlayerError {
    PlayerError::Http(format!("Your proxy {reason}"))
}

async fn connect_upstream(host: &str, port: u16) -> Result<TcpStream> {
    TcpStream::connect((host, port))
        .await
        .map_err(|e| upstream_error(format!("could not be reached: {e}")))
}

/// Opens a tunnel to `target_host:target_port` through a SOCKS5 proxy at `host:port`. Target
/// hostnames are sent to the proxy unresolved (ATYP `0x03`), so this computer never looks them
/// up itself; only literal IP addresses are sent as `0x01`/`0x04`.
pub async fn socks5_connect(
    host: &str,
    port: u16,
    username: Option<&str>,
    password: Option<&str>,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream> {
    let mut stream = connect_upstream(host, port).await?;

    let methods: &[u8] = if username.is_some() { &[0x02] } else { &[0x00] };
    let mut greeting = vec![0x05, methods.len() as u8];
    greeting.extend_from_slice(methods);
    stream
        .write_all(&greeting)
        .await
        .map_err(|e| upstream_error(format!("closed the connection: {e}")))?;

    let mut reply = [0u8; 2];
    stream
        .read_exact(&mut reply)
        .await
        .map_err(|e| upstream_error(format!("did not answer the greeting: {e}")))?;
    if reply[0] != 0x05 {
        return Err(upstream_error("is not a SOCKS5 proxy"));
    }
    match reply[1] {
        0x00 => {}
        0x02 => {
            let (Some(user), Some(pass)) = (username, password) else {
                return Err(upstream_error("asked for a username and password"));
            };
            let mut auth = vec![0x01, user.len() as u8];
            auth.extend_from_slice(user.as_bytes());
            auth.push(pass.len() as u8);
            auth.extend_from_slice(pass.as_bytes());
            stream
                .write_all(&auth)
                .await
                .map_err(|e| upstream_error(format!("closed the connection: {e}")))?;
            let mut auth_reply = [0u8; 2];
            stream
                .read_exact(&mut auth_reply)
                .await
                .map_err(|e| upstream_error(format!("did not answer the login: {e}")))?;
            if auth_reply[1] != 0x00 {
                return Err(upstream_error("rejected the username or password"));
            }
        }
        0xff => return Err(upstream_error("has no login method this app supports")),
        _ => {
            return Err(upstream_error(
                "asked for a login method this app does not support",
            ));
        }
    }

    let mut request = vec![0x05, 0x01, 0x00];
    match target_host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            request.push(0x01);
            request.extend_from_slice(&ip.octets());
        }
        Ok(IpAddr::V6(ip)) => {
            request.push(0x04);
            request.extend_from_slice(&ip.octets());
        }
        Err(_) => {
            let bytes = target_host.as_bytes();
            if bytes.len() > 255 {
                return Err(PlayerError::InvalidArg("That address is too long".into()));
            }
            request.push(0x03);
            request.push(bytes.len() as u8);
            request.extend_from_slice(bytes);
        }
    }
    request.extend_from_slice(&target_port.to_be_bytes());
    stream
        .write_all(&request)
        .await
        .map_err(|e| upstream_error(format!("closed the connection: {e}")))?;

    let mut head = [0u8; 4];
    stream
        .read_exact(&mut head)
        .await
        .map_err(|e| upstream_error(format!("did not answer the connect request: {e}")))?;
    if head[0] != 0x05 {
        return Err(upstream_error("sent an invalid reply"));
    }
    if head[1] != 0x00 {
        return Err(upstream_error(socks5_reply_reason(head[1])));
    }
    // Consume BND.ADDR + BND.PORT, whose length depends on ATYP; the value is unused.
    match head[3] {
        0x01 => {
            let mut rest = [0u8; 4 + 2];
            stream.read_exact(&mut rest).await.ok();
        }
        0x03 => {
            let mut len = [0u8; 1];
            if stream.read_exact(&mut len).await.is_ok() {
                let mut rest = vec![0u8; len[0] as usize + 2];
                stream.read_exact(&mut rest).await.ok();
            }
        }
        0x04 => {
            let mut rest = [0u8; 16 + 2];
            stream.read_exact(&mut rest).await.ok();
        }
        _ => {}
    }

    Ok(stream)
}

fn socks5_reply_reason(code: u8) -> &'static str {
    match code {
        0x01 => "reported a general failure",
        0x02 => "refused that address by its own rules",
        0x03 => "could not reach that network",
        0x04 => "could not reach that host",
        0x05 => "had the connection refused",
        0x06 => "took too long to answer",
        0x07 => "does not support that command",
        0x08 => "does not support that address type",
        _ => "refused the connection",
    }
}

fn basic_auth(username: &str, password: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
}

/// Opens a tunnel to `target_host:target_port` through an HTTP proxy at `host:port`, using the
/// proxy's own `CONNECT` method.
pub async fn http_connect(
    host: &str,
    port: u16,
    username: Option<&str>,
    password: Option<&str>,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream> {
    let mut stream = connect_upstream(host, port).await?;
    let auth = match (username, password) {
        (Some(user), Some(pass)) => {
            format!("Proxy-Authorization: Basic {}\r\n", basic_auth(user, pass))
        }
        _ => String::new(),
    };
    let request = format!(
        "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\n{auth}Connection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| upstream_error(format!("closed the connection: {e}")))?;
    read_http_connect_reply(&mut stream).await?;
    Ok(stream)
}

async fn read_http_connect_reply(stream: &mut TcpStream) -> Result<()> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while buf.len() < 16 * 1024 {
        match stream.read(&mut byte).await {
            Ok(0) => break,
            Ok(_) => {
                buf.push(byte[0]);
                if buf.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            Err(e) => return Err(upstream_error(format!("closed the connection: {e}"))),
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let status_line = text.lines().next().unwrap_or_default();
    let status = status_line.split_whitespace().nth(1).unwrap_or_default();
    if status.starts_with('2') {
        Ok(())
    } else if status == "407" {
        Err(upstream_error("rejected the username or password"))
    } else {
        Err(upstream_error(format!(
            "refused the connection ({status_line})"
        )))
    }
}

/// Connects to an HTTP proxy at `host:port` and forwards `request_head` (an absolute-form
/// request already rewritten for the proxy) as-is, returning the connected stream so its
/// response can be copied straight back to the original client.
pub async fn http_forward(
    host: &str,
    port: u16,
    username: Option<&str>,
    password: Option<&str>,
    request_head: &str,
) -> Result<TcpStream> {
    let mut stream = connect_upstream(host, port).await?;
    let mut head = request_head.to_string();
    if let (Some(user), Some(pass)) = (username, password) {
        let auth = format!("Proxy-Authorization: Basic {}\r\n", basic_auth(user, pass));
        // Insert right after the request line.
        if let Some(pos) = head.find("\r\n") {
            head.insert_str(pos + 2, &auth);
        }
    }
    stream
        .write_all(head.as_bytes())
        .await
        .map_err(|e| upstream_error(format!("closed the connection: {e}")))?;
    Ok(stream)
}

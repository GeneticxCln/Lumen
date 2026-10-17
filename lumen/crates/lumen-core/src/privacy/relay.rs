//! The local HTTP proxy every part of Lumen's own traffic points at. It accepts `CONNECT`
//! tunnels and absolute-form `http://` requests, decides per destination whether to connect
//! directly (a local-network source, like a VPN split-tunnel) or forward to the configured
//! upstream proxy, and otherwise just splices bytes between the client and wherever the
//! connection ends up. See `upstream` for the two proxy protocols it can forward to.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

use crate::error::{PlayerError, Result};
use crate::netguard;

use super::upstream::{http_connect, http_forward, socks5_connect};
use super::{Upstream, UpstreamScheme};

const MAX_HEAD_BYTES: usize = 16 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONNECTIONS: usize = 256;

struct RelayConfig {
    upstream: Upstream,
    password: Option<String>,
    allow_lan: bool,
}

/// A running relay. Dropping it stops the listener and aborts every open tunnel.
pub struct Relay {
    pub local_addr: SocketAddr,
    pub token: String,
    config: Arc<RwLock<RelayConfig>>,
    task: tokio::task::JoinHandle<()>,
}

impl Relay {
    /// Starts listening on `127.0.0.1` (a port chosen by the system).
    pub async fn start(
        upstream: Upstream,
        password: Option<String>,
        allow_lan: bool,
    ) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| PlayerError::Io(format!("Could not start the local proxy: {e}")))?;
        let local_addr = listener
            .local_addr()
            .map_err(|e| PlayerError::Io(e.to_string()))?;
        let token = random_token()?;

        let config = Arc::new(RwLock::new(RelayConfig {
            upstream,
            password,
            allow_lan,
        }));
        let shared_config = config.clone();
        let shared_token = token.clone();
        let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));

        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let config = shared_config.clone();
                let token = shared_token.clone();
                let slots = slots.clone();
                tokio::spawn(async move {
                    let Ok(_permit) = slots.try_acquire_owned() else {
                        return;
                    };
                    let _ = serve(stream, config, token).await;
                });
            }
        });

        Ok(Self {
            local_addr,
            token,
            config,
            task,
        })
    }

    pub fn set_upstream(&self, upstream: Upstream, password: Option<String>) {
        let mut config = self.config.write().unwrap_or_else(|p| p.into_inner());
        config.upstream = upstream;
        config.password = password;
    }

    pub fn set_allow_lan(&self, allow_lan: bool) {
        self.config
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .allow_lan = allow_lan;
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn random_token() -> Result<String> {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|e| PlayerError::Io(e.to_string()))?;
    Ok(raw.iter().map(|b| format!("{b:02x}")).collect())
}

/// What a connection asked for, decided without ever looking at the destination's identity
/// beyond what is needed to route it.
#[derive(Debug, PartialEq)]
pub enum Decision {
    /// Connect directly to this already-resolved address (a local-network source).
    Direct(SocketAddr),
    /// Forward to the configured upstream proxy; the hostname is not resolved here.
    Upstream,
    /// Refuse outright. The reason is safe to show (never includes the full address or a
    /// credential).
    Refuse(String),
}

fn decide_ip(addr: SocketAddr, allow_lan: bool) -> Decision {
    let ip = addr.ip();
    if !netguard::is_allowed_ip(ip, true) {
        return Decision::Refuse(format!("{ip} is a restricted address"));
    }
    if netguard::is_local_network(ip) {
        if allow_lan {
            Decision::Direct(addr)
        } else {
            Decision::Refuse(format!(
                "{ip} is a local address. Turn on local-network sources in Settings to use it."
            ))
        }
    } else {
        Decision::Upstream
    }
}

/// Decides how to route a request to `host:port`. A local name (`localhost`, `.local`,
/// `.internal`, a single unqualified label) is resolved here, since the whole point of the
/// bypass is to reach it without going out to the internet; anything else is left for the
/// upstream proxy to resolve, so this computer never looks up a destination's real name.
pub async fn decide(host: &str, port: u16, allow_lan: bool) -> Decision {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return decide_ip(SocketAddr::new(ip, port), allow_lan);
    }
    if netguard::is_local_name(host) {
        if !allow_lan {
            return Decision::Refuse(
                "is a local address. Turn on local-network sources in Settings to use it."
                    .to_string(),
            );
        }
        return match tokio::net::lookup_host((host, port)).await {
            Ok(addrs) => {
                match addrs
                    .into_iter()
                    .find(|addr| matches!(decide_ip(*addr, true), Decision::Direct(_)))
                {
                    Some(addr) => Decision::Direct(addr),
                    None => Decision::Refuse(format!("{host} did not resolve to a local address")),
                }
            }
            Err(_) => Decision::Refuse(format!("{host} could not be resolved")),
        };
    }
    Decision::Upstream
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Method {
    Connect,
    /// Absolute-form `http://...` request (the form reqwest, mpv and yt-dlp send for a plain
    /// `http://` target through a proxy).
    Forward,
}

struct ParsedRequest {
    method: Method,
    http_method: String,
    version: String,
    host: String,
    port: u16,
    /// Path plus query, for origin-form (talking directly to the destination, whether that is
    /// the real origin or a SOCKS5 tunnel that terminates there).
    path_and_query: String,
    /// The original absolute-form target (`http://host:port/path`), for forwarding to an HTTP
    /// upstream proxy, which (unlike a destination) expects absolute-form.
    absolute_target: String,
    header_lines: Vec<String>,
    proxy_authorization: Option<String>,
}

fn default_port(scheme: &str) -> u16 {
    if scheme == "https" { 443 } else { 80 }
}

/// Parses the request line and headers of a proxy request. `None` means it could not be
/// understood at all (not even enough to send a sensible error) and the connection should just
/// be closed.
fn parse_head(head: &str) -> Option<ParsedRequest> {
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split(' ');
    let http_method = parts.next()?.to_string();
    let target = parts.next()?;
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();

    let mut header_lines = Vec::new();
    let mut proxy_authorization = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("proxy-authorization")
        {
            proxy_authorization = Some(value.trim().to_string());
            continue;
        }
        header_lines.push(line.to_string());
    }

    if http_method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = target.rsplit_once(':')?;
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string();
        let port: u16 = port.parse().ok()?;
        return Some(ParsedRequest {
            method: Method::Connect,
            http_method,
            version,
            host,
            port,
            path_and_query: String::new(),
            absolute_target: String::new(),
            header_lines,
            proxy_authorization,
        });
    }

    if target.starts_with('/') {
        return None; // Origin-form: not a proxy request this relay will serve.
    }
    let url = url::Url::parse(target).ok()?;
    if url.scheme() != "http" {
        return None; // https/other absolute-form targets arrive as CONNECT instead.
    }
    let host = url.host_str()?.to_string();
    let port = url.port().unwrap_or_else(|| default_port(url.scheme()));
    let path_and_query = match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_string(),
    };
    Some(ParsedRequest {
        method: Method::Forward,
        http_method,
        version,
        host,
        port,
        path_and_query,
        absolute_target: target.to_string(),
        header_lines,
        proxy_authorization,
    })
}

/// This only gates a local proxy only this process knows the token for, not a remote secret,
/// but there is no reason not to compare the whole value either way.
fn check_auth(proxy_authorization: Option<&str>, token: &str) -> bool {
    use base64::Engine;
    let Some(value) = proxy_authorization else {
        return false;
    };
    let Some(encoded) = value.strip_prefix("Basic ") else {
        return false;
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return false;
    };
    decoded == format!("lumen:{token}").as_bytes()
}

async fn read_head(reader: &mut BufReader<TcpStream>) -> std::io::Result<String> {
    let mut head = Vec::new();
    loop {
        if head.len() >= MAX_HEAD_BYTES {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let mut line = Vec::new();
        let n = reader.read_until(b'\n', &mut line).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        let blank = line == b"\r\n" || line == b"\n";
        head.extend_from_slice(&line);
        if blank {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

async fn reply(stream: &mut TcpStream, status: &str, extra_headers: &str) -> std::io::Result<()> {
    stream
        .write_all(
            format!("HTTP/1.1 {status}\r\n{extra_headers}Connection: close\r\n\r\n").as_bytes(),
        )
        .await
}

/// Appends rewritten headers (hop-by-hop and hop-only headers stripped, `Host` added if
/// missing, connection closed after one request) to `out`, which should already hold a request
/// line ending in `\r\n`.
fn append_rewritten_headers(out: &mut String, request: &ParsedRequest) {
    let mut has_host = false;
    for line in &request.header_lines {
        let Some((name, _)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.eq_ignore_ascii_case("proxy-connection")
            || name.eq_ignore_ascii_case("connection")
            || name.eq_ignore_ascii_case("keep-alive")
        {
            continue;
        }
        if name.eq_ignore_ascii_case("host") {
            has_host = true;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    if !has_host {
        out.push_str(&format!("Host: {}\r\n", request.host));
    }
    out.push_str("Connection: close\r\n\r\n");
}

/// The request as sent straight to a destination (the real origin, or the far end of a SOCKS5
/// tunnel, which behaves the same way once connected).
fn origin_form_head(request: &ParsedRequest) -> String {
    let mut out = format!(
        "{} {} {}\r\n",
        request.http_method, request.path_and_query, request.version
    );
    append_rewritten_headers(&mut out, request);
    out
}

/// The request as sent to an HTTP upstream proxy, which (unlike a destination) expects
/// absolute-form; `http_forward` adds the proxy's own `Proxy-Authorization`.
fn absolute_form_head(request: &ParsedRequest) -> String {
    let mut out = format!(
        "{} {} {}\r\n",
        request.http_method, request.absolute_target, request.version
    );
    append_rewritten_headers(&mut out, request);
    out
}

async fn serve(
    stream: TcpStream,
    config: Arc<RwLock<RelayConfig>>,
    token: String,
) -> std::io::Result<()> {
    stream.set_nodelay(true).ok();
    let mut reader = BufReader::new(stream);
    let head = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut reader)).await {
        Ok(Ok(head)) => head,
        _ => return Ok(()),
    };
    let Some(request) = parse_head(&head) else {
        return reply(reader.get_mut(), "400 Bad Request", "").await;
    };
    if !check_auth(request.proxy_authorization.as_deref(), &token) {
        return reply(
            reader.get_mut(),
            "407 Proxy Authentication Required",
            "Proxy-Authenticate: Basic realm=\"Lumen\"\r\n",
        )
        .await;
    }

    let (allow_lan, upstream, password) = {
        let config = config.read().unwrap_or_else(|p| p.into_inner());
        (
            config.allow_lan,
            config.upstream.clone(),
            config.password.clone(),
        )
    };

    let decision = decide(&request.host, request.port, allow_lan).await;
    let connect_result = match &decision {
        Decision::Refuse(reason) => Err(reason.clone()),
        Decision::Direct(addr) => tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(*addr))
            .await
            .map_err(|_| "timed out".to_string())
            .and_then(|r| r.map_err(|e| e.to_string())),
        Decision::Upstream => connect_upstream(&upstream, password.as_deref(), &request)
            .await
            .map_err(String::from),
    };

    let mut origin = match connect_result {
        Ok(stream) => stream,
        Err(reason) => {
            let status = if matches!(decision, Decision::Refuse(_)) {
                "403 Forbidden"
            } else {
                "502 Bad Gateway"
            };
            return reply(
                reader.get_mut(),
                status,
                &format!("X-Lumen-Refused: {reason}\r\n"),
            )
            .await;
        }
    };

    match request.method {
        Method::Connect => {
            reader
                .get_mut()
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
        }
        Method::Forward => {
            // A direct connection, or a SOCKS5 tunnel (both land straight on the destination),
            // get origin-form; an HTTP upstream proxy already received its absolute-form
            // request inside `connect_upstream`/`http_forward`.
            let already_sent =
                matches!(decision, Decision::Upstream) && upstream.scheme == UpstreamScheme::Http;
            if !already_sent {
                origin
                    .write_all(origin_form_head(&request).as_bytes())
                    .await?;
            }
        }
    }

    tokio::io::copy_bidirectional(&mut reader, &mut origin)
        .await
        .map(|_| ())
        .or(Ok(()))
}

/// Opens, and immediately lets the caller drop, a connection through `upstream` to a host
/// Lumen already trusts (the same one self-update checks against) — proof the proxy itself
/// works without starting the local relay.
pub async fn test_connect(upstream: &Upstream, password: Option<&str>) -> Result<()> {
    let request = ParsedRequest {
        method: Method::Connect,
        http_method: "CONNECT".into(),
        version: "HTTP/1.1".into(),
        host: "api.github.com".into(),
        port: 443,
        path_and_query: String::new(),
        absolute_target: String::new(),
        header_lines: Vec::new(),
        proxy_authorization: None,
    };
    connect_upstream(upstream, password, &request)
        .await
        .map(|_| ())
}

async fn connect_upstream(
    upstream: &Upstream,
    password: Option<&str>,
    request: &ParsedRequest,
) -> Result<TcpStream> {
    let connect = async {
        match (upstream.scheme, request.method) {
            (UpstreamScheme::Socks5, _) => {
                socks5_connect(
                    &upstream.host,
                    upstream.port,
                    upstream.username.as_deref(),
                    password,
                    &request.host,
                    request.port,
                )
                .await
            }
            (UpstreamScheme::Http, Method::Connect) => {
                http_connect(
                    &upstream.host,
                    upstream.port,
                    upstream.username.as_deref(),
                    password,
                    &request.host,
                    request.port,
                )
                .await
            }
            (UpstreamScheme::Http, Method::Forward) => {
                http_forward(
                    &upstream.host,
                    upstream.port,
                    upstream.username.as_deref(),
                    password,
                    &absolute_form_head(request),
                )
                .await
            }
        }
    };
    tokio::time::timeout(CONNECT_TIMEOUT, connect)
        .await
        .map_err(|_| PlayerError::Http("Your proxy took too long to answer".into()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncReadExt;

    // ---------- pure function tests ----------

    #[test]
    fn decide_ip_classifies_addresses() {
        assert_eq!(
            decide_ip("8.8.8.8:80".parse().unwrap(), false),
            Decision::Upstream
        );
        assert_eq!(
            decide_ip("8.8.8.8:80".parse().unwrap(), true),
            Decision::Upstream
        );
        assert!(matches!(
            decide_ip("192.168.1.5:80".parse().unwrap(), true),
            Decision::Direct(_)
        ));
        assert!(matches!(
            decide_ip("192.168.1.5:80".parse().unwrap(), false),
            Decision::Refuse(_)
        ));
        assert!(
            matches!(
                decide_ip("169.254.169.254:80".parse().unwrap(), true),
                Decision::Refuse(_)
            ),
            "the cloud metadata address is refused even with allow_lan"
        );
    }

    #[tokio::test]
    async fn decide_resolves_local_names_but_leaves_public_ones_unresolved() {
        assert_eq!(
            decide("example.com", 80, true).await,
            Decision::Upstream,
            "a public name must not be looked up here"
        );
        assert!(matches!(
            decide("localhost", 80, false).await,
            Decision::Refuse(_)
        ));
        assert!(matches!(
            decide("localhost", 80, true).await,
            Decision::Direct(_)
        ));
    }

    #[test]
    fn parse_head_understands_connect_and_absolute_form() {
        let connect = parse_head(
            "CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\nProxy-Authorization: Basic abc\r\n\r\n",
        )
        .unwrap();
        assert_eq!(connect.method, Method::Connect);
        assert_eq!(connect.host, "example.com");
        assert_eq!(connect.port, 443);
        assert_eq!(connect.proxy_authorization.as_deref(), Some("Basic abc"));

        let forward =
            parse_head("GET http://example.com:8080/a/b?x=1 HTTP/1.1\r\nHost: ignored\r\n\r\n")
                .unwrap();
        assert_eq!(forward.method, Method::Forward);
        assert_eq!(forward.host, "example.com");
        assert_eq!(forward.port, 8080);
        assert_eq!(forward.path_and_query, "/a/b?x=1");
        assert_eq!(forward.absolute_target, "http://example.com:8080/a/b?x=1");

        assert!(
            parse_head("GET /just/a/path HTTP/1.1\r\n\r\n").is_none(),
            "origin-form is refused"
        );
        assert!(
            parse_head("GET https://example.com/ HTTP/1.1\r\n\r\n").is_none(),
            "https absolute-form never arrives this way; it comes as CONNECT"
        );
        assert!(parse_head("").is_none());
        assert!(parse_head("not a request line at all\r\n\r\n").is_none());
    }

    #[test]
    fn a_bracketed_ipv6_connect_target_parses() {
        let connect = parse_head("CONNECT [::1]:443 HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(connect.host, "::1");
        assert_eq!(connect.port, 443);
    }

    #[test]
    fn check_auth_requires_the_exact_token() {
        use base64::Engine;
        let encode = |s: &str| {
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(s)
            )
        };
        assert!(check_auth(Some(&encode("lumen:tok")), "tok"));
        assert!(!check_auth(Some(&encode("lumen:wrong")), "tok"));
        assert!(!check_auth(None, "tok"));
        assert!(!check_auth(Some("not base64 at all!!"), "tok"));
        assert!(!check_auth(
            Some(&encode("lumen:tok").replacen("Basic", "Bearer", 1)),
            "tok"
        ));
    }

    #[test]
    fn header_rewriting_drops_hop_by_hop_headers_and_closes_the_connection() {
        let request = parse_head(
            "GET http://example.com/x HTTP/1.1\r\nHost: example.com\r\nConnection: keep-alive\r\nProxy-Connection: keep-alive\r\nUser-Agent: test\r\n\r\n",
        )
        .unwrap();
        let origin = origin_form_head(&request);
        assert!(origin.starts_with("GET /x HTTP/1.1\r\n"), "{origin}");
        assert!(origin.contains("Host: example.com"));
        assert!(origin.contains("User-Agent: test"));
        assert!(!origin.contains("Proxy-Connection"));
        assert_eq!(
            origin.matches("Connection:").count(),
            1,
            "the original keep-alive is replaced, not kept alongside close: {origin}"
        );
        assert!(origin.contains("Connection: close"));

        let absolute = absolute_form_head(&request);
        assert!(
            absolute.starts_with("GET http://example.com/x HTTP/1.1\r\n"),
            "{absolute}"
        );
    }

    #[test]
    fn a_missing_host_header_is_added() {
        let request = parse_head("GET http://example.com/x HTTP/1.1\r\n\r\n").unwrap();
        assert!(origin_form_head(&request).contains("Host: example.com"));
    }

    // ---------- end-to-end tests against fake upstreams and origins ----------

    async fn start_fake_origin(body: &'static str) -> (SocketAddr, Arc<AtomicUsize>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let shared = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                shared.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        (addr, hits)
    }

    #[derive(Debug, Clone)]
    struct Socks5Seen {
        atyp: u8,
        host: String,
        username: Option<String>,
    }

    struct FakeSocks5 {
        local_addr: SocketAddr,
        seen: Arc<Mutex<Vec<Socks5Seen>>>,
    }

    impl FakeSocks5 {
        /// `require_auth`, when set, is the `(username, password)` this fake insists on.
        /// Every accepted connection is relayed on to `origin`, regardless of what host and
        /// port it was asked to reach — the test only cares what was asked, not where it leads.
        async fn start(
            require_auth: Option<(&'static str, &'static str)>,
            origin: SocketAddr,
        ) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let local_addr = listener.local_addr().unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let shared = seen.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        break;
                    };
                    let shared = shared.clone();
                    tokio::spawn(async move {
                        let _ = serve_fake_socks5(&mut stream, require_auth, origin, shared).await;
                    });
                }
            });
            Self { local_addr, seen }
        }

        fn seen(&self) -> Vec<Socks5Seen> {
            self.seen.lock().unwrap().clone()
        }
    }

    async fn serve_fake_socks5(
        stream: &mut TcpStream,
        require_auth: Option<(&str, &str)>,
        origin: SocketAddr,
        seen: Arc<Mutex<Vec<Socks5Seen>>>,
    ) -> std::io::Result<()> {
        let mut greeting = [0u8; 2];
        stream.read_exact(&mut greeting).await?;
        let mut methods = vec![0u8; greeting[1] as usize];
        stream.read_exact(&mut methods).await?;

        let mut username = None;
        if let Some((want_user, want_pass)) = require_auth {
            stream.write_all(&[0x05, 0x02]).await?;
            let mut head = [0u8; 2];
            stream.read_exact(&mut head).await?;
            let mut user = vec![0u8; head[1] as usize];
            stream.read_exact(&mut user).await?;
            let mut plen = [0u8; 1];
            stream.read_exact(&mut plen).await?;
            let mut pass = vec![0u8; plen[0] as usize];
            stream.read_exact(&mut pass).await?;
            let (user_s, pass_s) = (
                String::from_utf8_lossy(&user).into_owned(),
                String::from_utf8_lossy(&pass).into_owned(),
            );
            if user_s == want_user && pass_s == want_pass {
                stream.write_all(&[0x01, 0x00]).await?;
                username = Some(user_s);
            } else {
                stream.write_all(&[0x01, 0x01]).await?;
                return Ok(());
            }
        } else {
            stream.write_all(&[0x05, 0x00]).await?;
        }

        let mut head = [0u8; 4];
        stream.read_exact(&mut head).await?;
        let atyp = head[3];
        let host = match atyp {
            0x01 => {
                let mut b = [0u8; 4];
                stream.read_exact(&mut b).await?;
                IpAddr::from(b).to_string()
            }
            0x03 => {
                let mut len = [0u8; 1];
                stream.read_exact(&mut len).await?;
                let mut b = vec![0u8; len[0] as usize];
                stream.read_exact(&mut b).await?;
                String::from_utf8_lossy(&b).into_owned()
            }
            0x04 => {
                let mut b = [0u8; 16];
                stream.read_exact(&mut b).await?;
                IpAddr::from(b).to_string()
            }
            _ => return Ok(()),
        };
        let mut port = [0u8; 2];
        stream.read_exact(&mut port).await?;

        seen.lock().unwrap().push(Socks5Seen {
            atyp,
            host,
            username,
        });

        stream
            .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;

        let mut to_origin = TcpStream::connect(origin).await?;
        tokio::io::copy_bidirectional(stream, &mut to_origin)
            .await
            .ok();
        Ok(())
    }

    /// Captures the request head it receives and answers with a canned `200 OK`; used to check
    /// exactly what the relay sent an HTTP upstream without needing a real one.
    async fn start_capturing_http_upstream() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let shared = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let shared = shared.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(stream);
                    let mut head = Vec::new();
                    loop {
                        let mut line = Vec::new();
                        let n = reader.read_until(b'\n', &mut line).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        let blank = line == b"\r\n" || line == b"\n";
                        head.extend_from_slice(&line);
                        if blank {
                            break;
                        }
                    }
                    shared
                        .lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&head).into_owned());
                    let _ = reader
                        .get_mut()
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        )
                        .await;
                });
            }
        });
        (addr, seen)
    }

    async fn connect_with_auth(relay_addr: SocketAddr, request: &str, token: &str) -> TcpStream {
        use base64::Engine;
        let mut stream = TcpStream::connect(relay_addr).await.unwrap();
        let header = format!(
            "Proxy-Authorization: Basic {}\r\n",
            base64::engine::general_purpose::STANDARD.encode(format!("lumen:{token}"))
        );
        let mut full = request.to_string();
        let pos = full.find("\r\n\r\n").unwrap();
        full.insert_str(pos + 2, &header);
        stream.write_all(full.as_bytes()).await.unwrap();
        stream
    }

    async fn read_head_only(stream: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = stream.read(&mut byte).await.unwrap();
            if n == 0 {
                break;
            }
            buf.push(byte[0]);
            if buf.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    async fn send_head_only(relay_addr: SocketAddr, request: &str, token: &str) -> String {
        let mut stream = connect_with_auth(relay_addr, request, token).await;
        read_head_only(&mut stream).await
    }

    async fn send_and_read_all(relay_addr: SocketAddr, request: &str, token: &str) -> String {
        let mut stream = connect_with_auth(relay_addr, request, token).await;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.ok();
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[tokio::test]
    async fn a_request_reaches_the_origin_via_the_socks5_upstream_with_the_hostname_unresolved() {
        let (origin_addr, origin_hits) = start_fake_origin("hello").await;
        let fake_upstream = FakeSocks5::start(None, origin_addr).await;
        let upstream = Upstream {
            scheme: UpstreamScheme::Socks5,
            host: fake_upstream.local_addr.ip().to_string(),
            port: fake_upstream.local_addr.port(),
            username: None,
        };
        let relay = Relay::start(upstream, None, false).await.unwrap();

        let response = send_and_read_all(
            relay.local_addr,
            &format!(
                "GET http://media.invalid:{}/x HTTP/1.1\r\n\r\n",
                origin_addr.port()
            ),
            &relay.token,
        )
        .await;

        assert!(response.contains("200 OK"), "{response}");
        assert!(response.ends_with("hello"), "{response}");
        assert_eq!(origin_hits.load(Ordering::SeqCst), 1);
        let seen = fake_upstream.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].atyp, 0x03,
            "the hostname must be sent unresolved (ATYP domain)"
        );
        assert_eq!(seen[0].host, "media.invalid");
    }

    #[tokio::test]
    async fn socks5_credentials_reach_the_upstream() {
        let (origin_addr, _hits) = start_fake_origin("ok").await;
        let fake_upstream = FakeSocks5::start(Some(("alice", "secret")), origin_addr).await;
        let upstream = Upstream {
            scheme: UpstreamScheme::Socks5,
            host: fake_upstream.local_addr.ip().to_string(),
            port: fake_upstream.local_addr.port(),
            username: Some("alice".into()),
        };
        let relay = Relay::start(upstream, Some("secret".into()), false)
            .await
            .unwrap();

        let response = send_head_only(
            relay.local_addr,
            "CONNECT media.invalid:443 HTTP/1.1\r\n\r\n",
            &relay.token,
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert_eq!(fake_upstream.seen()[0].username.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn an_unreachable_upstream_never_falls_back_to_a_direct_connection() {
        let (origin_addr, origin_hits) = start_fake_origin("unreachable-test").await;
        let dead_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let dead_addr = dead_listener.local_addr().unwrap();
        drop(dead_listener); // nothing listens here now

        let upstream = Upstream {
            scheme: UpstreamScheme::Socks5,
            host: dead_addr.ip().to_string(),
            port: dead_addr.port(),
            username: None,
        };
        let relay = Relay::start(upstream, None, false).await.unwrap();

        let response = send_head_only(
            relay.local_addr,
            &format!(
                "GET http://media.invalid:{}/x HTTP/1.1\r\n\r\n",
                origin_addr.port()
            ),
            &relay.token,
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 502"), "{response}");
        assert_eq!(
            origin_hits.load(Ordering::SeqCst),
            0,
            "must never connect directly when the upstream fails"
        );
    }

    #[tokio::test]
    async fn a_lan_target_connects_directly_and_never_touches_the_upstream() {
        let (origin_addr, origin_hits) = start_fake_origin("lan").await;
        // Would succeed if ever used; the test proves it is not.
        let fake_upstream = FakeSocks5::start(None, origin_addr).await;
        let upstream = Upstream {
            scheme: UpstreamScheme::Socks5,
            host: fake_upstream.local_addr.ip().to_string(),
            port: fake_upstream.local_addr.port(),
            username: None,
        };
        let relay = Relay::start(upstream, None, true).await.unwrap();

        let response = send_and_read_all(
            relay.local_addr,
            &format!("GET http://{origin_addr} HTTP/1.1\r\n\r\n"),
            &relay.token,
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert_eq!(origin_hits.load(Ordering::SeqCst), 1);
        assert!(
            fake_upstream.seen().is_empty(),
            "a local-network target must bypass the upstream proxy entirely"
        );
    }

    #[tokio::test]
    async fn a_lan_target_is_refused_without_allow_lan() {
        let (origin_addr, origin_hits) = start_fake_origin("lan").await;
        let upstream = Upstream {
            scheme: UpstreamScheme::Socks5,
            host: "127.0.0.1".into(),
            port: 1,
            username: None,
        };
        let relay = Relay::start(upstream, None, false).await.unwrap();

        let response = send_head_only(
            relay.local_addr,
            &format!("GET http://{origin_addr} HTTP/1.1\r\n\r\n"),
            &relay.token,
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        assert_eq!(origin_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_missing_or_wrong_credential_is_rejected_with_407() {
        let upstream = Upstream {
            scheme: UpstreamScheme::Socks5,
            host: "127.0.0.1".into(),
            port: 1,
            username: None,
        };
        let relay = Relay::start(upstream, None, false).await.unwrap();

        let mut stream = TcpStream::connect(relay.local_addr).await.unwrap();
        stream
            .write_all(b"GET http://example.com/ HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let response = read_head_only(&mut stream).await;
        assert!(response.starts_with("HTTP/1.1 407"), "{response}");
        assert!(response.contains("Proxy-Authenticate: Basic"), "{response}");
    }

    #[tokio::test]
    async fn http_upstream_forward_uses_absolute_form_with_the_upstreams_own_credentials() {
        let (upstream_addr, seen) = start_capturing_http_upstream().await;
        let upstream = Upstream {
            scheme: UpstreamScheme::Http,
            host: upstream_addr.ip().to_string(),
            port: upstream_addr.port(),
            username: Some("bob".into()),
        };
        let relay = Relay::start(upstream, Some("hunter2".into()), false)
            .await
            .unwrap();

        let response = send_and_read_all(
            relay.local_addr,
            "GET http://example.com/path HTTP/1.1\r\n\r\n",
            &relay.token,
        )
        .await;
        assert!(response.contains("200 OK"), "{response}");

        let captured = seen.lock().unwrap().clone();
        assert_eq!(captured.len(), 1);
        assert!(
            captured[0].starts_with("GET http://example.com/path HTTP/1.1\r\n"),
            "{}",
            captured[0]
        );
        assert!(
            captured[0].contains("Proxy-Authorization: Basic"),
            "{}",
            captured[0]
        );
        assert!(
            !captured[0].contains(&relay.token),
            "the relay's own client-facing token must never reach the upstream: {}",
            captured[0]
        );
    }

    #[tokio::test]
    async fn http_upstream_connect_tunnels_and_never_leaks_the_relays_own_token() {
        let (upstream_addr, seen) = start_capturing_http_upstream().await;
        let upstream = Upstream {
            scheme: UpstreamScheme::Http,
            host: upstream_addr.ip().to_string(),
            port: upstream_addr.port(),
            username: None,
        };
        let relay = Relay::start(upstream, None, false).await.unwrap();

        let response = send_head_only(
            relay.local_addr,
            "CONNECT example.com:443 HTTP/1.1\r\n\r\n",
            &relay.token,
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");

        let captured = seen.lock().unwrap().clone();
        assert_eq!(captured.len(), 1);
        assert!(
            captured[0].starts_with("CONNECT example.com:443"),
            "{}",
            captured[0]
        );
        assert!(!captured[0].contains(&relay.token));
    }

    #[tokio::test]
    async fn dropping_the_relay_stops_the_listener() {
        let upstream = Upstream {
            scheme: UpstreamScheme::Socks5,
            host: "127.0.0.1".into(),
            port: 1,
            username: None,
        };
        let relay = Relay::start(upstream, None, false).await.unwrap();
        let addr = relay.local_addr;
        drop(relay);
        // Give the aborted task a moment, then confirm nothing answers any more.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(TcpStream::connect(addr).await.is_err());
    }
}

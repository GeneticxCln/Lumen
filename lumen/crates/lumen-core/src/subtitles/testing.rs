//! A tiny in-process HTTP server for the provider tests: canned answers by path, a count of the
//! requests it received, and nothing else.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub struct Canned {
    path: String,
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    /// `Location` header, for redirect tests.
    location: Option<String>,
}

impl Canned {
    pub fn json(path: &str, body: &str) -> Self {
        Self::new(path, 200, "application/json", body.as_bytes().to_vec())
    }

    pub fn text(path: &str, body: &str) -> Self {
        Self::new(path, 200, "text/plain", body.as_bytes().to_vec())
    }

    pub fn bytes(path: &str, body: Vec<u8>) -> Self {
        Self::new(path, 200, "application/octet-stream", body)
    }

    pub fn status(path: &str, status: u16) -> Self {
        Self::new(path, status, "text/plain", Vec::new())
    }

    pub fn redirect(path: &str, to: &str) -> Self {
        let mut canned = Self::new(path, 302, "text/plain", Vec::new());
        canned.location = Some(to.to_string());
        canned
    }

    fn new(path: &str, status: u16, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            path: path.into(),
            status,
            content_type,
            body,
            location: None,
        }
    }
}

pub struct Server {
    /// `http://127.0.0.1:port`
    pub base: String,
    hits: Arc<AtomicUsize>,
}

impl Server {
    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

/// Answers every request whose path (without the query) is in `routes`, and 404 for the rest.
/// `__BASE__` in a body is replaced with the server's own address.
pub async fn serve(routes: Vec<Canned>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let table: Arc<HashMap<String, Canned>> = Arc::new(
        routes
            .into_iter()
            .map(|route| (route.path.clone(), route))
            .collect(),
    );

    let counter = Arc::clone(&hits);
    let own = base.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let table = Arc::clone(&table);
            let counter = Arc::clone(&counter);
            let own = own.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0u8; 2048];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => request.extend_from_slice(&buffer[..n]),
                    }
                }
                counter.fetch_add(1, Ordering::SeqCst);
                let head = String::from_utf8_lossy(&request);
                let target = head.split_whitespace().nth(1).unwrap_or("/");
                let path = target.split('?').next().unwrap_or("/");
                let reply = match table.get(path) {
                    Some(route) => {
                        let body = if route.content_type == "application/octet-stream" {
                            route.body.clone()
                        } else {
                            String::from_utf8_lossy(&route.body)
                                .replace("__BASE__", &own)
                                .into_bytes()
                        };
                        let mut head = format!(
                            "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                            route.status,
                            route.content_type,
                            body.len()
                        );
                        if let Some(location) = &route.location {
                            head.push_str(&format!(
                                "Location: {}\r\n",
                                location.replace("__BASE__", &own)
                            ));
                        }
                        head.push_str("\r\n");
                        let mut bytes = head.into_bytes();
                        bytes.extend_from_slice(&body);
                        bytes
                    }
                    None => {
                        b"HTTP/1.1 404 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
                    }
                };
                let _ = stream.write_all(&reply).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    Server { base, hits }
}

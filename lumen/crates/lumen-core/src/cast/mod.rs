//! Sending what is playing to a TV or speaker on the local network (UPnP/DLNA media renderers).
//!
//! The pieces: `ssdp` finds devices, `description` reads what they say about themselves, `soap`
//! builds and reads the `AVTransport` calls, and `server` hands a local file to the device.
//! Everything here stays on the local network: a device that announces an address outside it is
//! ignored, and the files served are only the ones the user sent, to the one device they went to.
//! Not a Chromecast: that is a different protocol.

pub mod description;
pub mod server;
pub mod soap;
pub mod ssdp;

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use serde::Serialize;
use url::{Host, Url};

use crate::error::{PlayerError, Result, redacted};
use crate::netguard;
use server::MediaServer;

const CALL_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ANSWER_BYTES: usize = 256 * 1024;

/// A device that can be sent a video.
#[derive(Debug, Clone, Serialize)]
pub struct Renderer {
    /// The device's unique id; what the page passes back to pick it.
    pub id: String,
    pub name: String,
    /// Its address on the network, shown to tell two devices of the same name apart.
    pub address: String,
    #[serde(skip)]
    control_url: Url,
    #[serde(skip)]
    service_type: String,
}

/// What to play.
#[derive(Debug, Clone)]
pub enum Source {
    /// An address the device fetches itself.
    Url(String),
    /// A file on this computer, served to the device.
    File(PathBuf),
}

/// What the device reports about the playback.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Status {
    /// `PLAYING`, `PAUSED_PLAYBACK`, `STOPPED`, `TRANSITIONING`, …
    pub state: String,
    pub position: f64,
    pub duration: f64,
}

/// Whether `ip` is an address on a local network (private, link-local or loopback) and not on the
/// internet: the only place a renderer may be.
pub fn is_local_network(ip: IpAddr) -> bool {
    netguard::is_allowed_ip(ip, true) && !netguard::is_allowed_ip(ip, false)
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(CALL_TIMEOUT)
            // A renderer has no business redirecting; and LAN calls must not go through a proxy.
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .expect("the HTTP client has valid settings")
    })
}

fn local_ip(url: &Url) -> Option<IpAddr> {
    match url.host()? {
        Host::Ipv4(ip) => Some(IpAddr::V4(ip)).filter(|ip| is_local_network(*ip)),
        Host::Ipv6(ip) => Some(IpAddr::V6(ip)).filter(|ip| is_local_network(*ip)),
        Host::Domain(_) => None,
    }
}

async fn read_limited(mut response: reqwest::Response) -> Result<String> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| PlayerError::Http(redacted(e)))?
    {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_ANSWER_BYTES {
            return Err(PlayerError::Http("The device sent too much data".into()));
        }
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

async fn describe(location: Url) -> Result<Renderer> {
    let ip = local_ip(&location).ok_or_else(|| {
        PlayerError::InvalidArg("A device announced an address outside the local network".into())
    })?;
    let response = client()
        .get(location.clone())
        .send()
        .await
        .map_err(|e| PlayerError::Http(redacted(e)))?;
    if !response.status().is_success() {
        return Err(PlayerError::Http(format!(
            "The device answered {}",
            response.status()
        )));
    }
    let description = description::parse(&read_limited(response).await?, &location)?;
    Ok(Renderer {
        id: description.udn,
        name: description.name,
        address: ip.to_string(),
        control_url: description.control_url,
        service_type: description.service_type,
    })
}

/// Looks for devices on the local network for `wait`.
pub async fn discover(wait: Duration) -> Result<Vec<Renderer>> {
    discover_at(ssdp::MULTICAST, wait).await
}

/// `discover` with the search sent to another address (a test device).
pub async fn discover_at(target: std::net::SocketAddr, wait: Duration) -> Result<Vec<Renderer>> {
    let replies = ssdp::search(target, ssdp::MEDIA_RENDERER, wait).await?;
    let mut tasks = tokio::task::JoinSet::new();
    for reply in replies {
        tasks.spawn(describe(reply.location));
    }
    let mut found: Vec<Renderer> = Vec::new();
    while let Some(done) = tasks.join_next().await {
        // A device that does not describe itself properly is skipped, not an error for the rest.
        if let Ok(Ok(renderer)) = done
            && !found.iter().any(|known| known.id == renderer.id)
        {
            found.push(renderer);
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

impl Renderer {
    async fn call(&self, action: &str, arguments: &[(&str, &str)]) -> Result<String> {
        // The address was checked when the device was found; it is checked again here because a
        // `Renderer` can be built from stored data.
        if local_ip(&self.control_url).is_none() {
            return Err(PlayerError::InvalidArg(
                "The device is not on the local network".into(),
            ));
        }
        let response = client()
            .post(self.control_url.clone())
            .header("Content-Type", "text/xml; charset=\"utf-8\"")
            .header(
                "SOAPACTION",
                soap::action_header(&self.service_type, action),
            )
            .body(soap::envelope(&self.service_type, action, arguments))
            .send()
            .await
            .map_err(|e| PlayerError::Http(redacted(e)))?;
        let status = response.status();
        let body = read_limited(response).await?;
        if let Some(reason) = soap::fault(&body) {
            return Err(PlayerError::Http(format!(
                "{} refused {action}: {reason}",
                self.name
            )));
        }
        if !status.is_success() {
            return Err(PlayerError::Http(format!(
                "{} answered {status} to {action}",
                self.name
            )));
        }
        Ok(body)
    }

    pub async fn play(&self) -> Result<()> {
        self.call("Play", &[("InstanceID", "0"), ("Speed", "1")])
            .await
            .map(drop)
    }

    pub async fn pause(&self) -> Result<()> {
        self.call("Pause", &[("InstanceID", "0")]).await.map(drop)
    }

    pub async fn stop(&self) -> Result<()> {
        self.call("Stop", &[("InstanceID", "0")]).await.map(drop)
    }

    pub async fn seek(&self, seconds: f64) -> Result<()> {
        let target = soap::clock(seconds);
        self.call(
            "Seek",
            &[
                ("InstanceID", "0"),
                ("Unit", "REL_TIME"),
                ("Target", &target),
            ],
        )
        .await
        .map(drop)
    }

    pub async fn status(&self) -> Result<Status> {
        let transport = self
            .call("GetTransportInfo", &[("InstanceID", "0")])
            .await?;
        let position = self.call("GetPositionInfo", &[("InstanceID", "0")]).await?;
        Ok(Status {
            state: soap::field(&transport, "CurrentTransportState").unwrap_or_default(),
            position: soap::field(&position, "RelTime")
                .and_then(|t| soap::parse_clock(&t))
                .unwrap_or(0.0),
            duration: soap::field(&position, "TrackDuration")
                .and_then(|t| soap::parse_clock(&t))
                .unwrap_or(0.0),
        })
    }
}

/// A video on its way to a device. Dropping it stops serving the file; it does not stop the
/// device (call `stop`).
pub struct Session {
    pub renderer: Renderer,
    _server: Option<MediaServer>,
}

/// Sends `source` to `renderer` and starts it, at `start_at` seconds when that is past the
/// beginning. `allow_lan` is the user's setting for sources on the local network.
pub async fn start(
    renderer: &Renderer,
    source: Source,
    title: &str,
    start_at: f64,
    allow_lan: bool,
) -> Result<Session> {
    let device_ip = local_ip(&renderer.control_url)
        .ok_or_else(|| PlayerError::InvalidArg("The device is not on the local network".into()))?;
    let (address, mime, media_server) = match source {
        Source::Url(raw) => {
            let url = netguard::validate_url(&raw, allow_lan)?;
            let mime = server::mime_for(std::path::Path::new(url.path()));
            let mime = if mime == "application/octet-stream" {
                "video/mp4"
            } else {
                mime
            };
            (url.to_string(), mime, None)
        }
        Source::File(path) => {
            // Only media: this server must not become a way to read any file on the computer.
            if server::mime_for(&path) == "application/octet-stream" {
                return Err(PlayerError::InvalidArg(
                    "That kind of file cannot be sent to a device".into(),
                ));
            }
            let bind = server::local_address_for(device_ip)?;
            let media_server = MediaServer::start(bind, device_ip).await?;
            let address = media_server.add(&path)?;
            (address, server::mime_for(&path), Some(media_server))
        }
    };
    // A device that is playing something else refuses a new address until it is stopped.
    let _ = renderer.stop().await;
    renderer
        .call(
            "SetAVTransportURI",
            &[
                ("InstanceID", "0"),
                ("CurrentURI", &address),
                ("CurrentURIMetaData", &soap::didl(title, &address, mime)),
            ],
        )
        .await?;
    renderer.play().await?;
    if start_at >= 2.0 {
        // Seeking works once the device has started; give it a few tries.
        for _ in 0..6 {
            tokio::time::sleep(Duration::from_millis(600)).await;
            if renderer.seek(start_at).await.is_ok() {
                break;
            }
        }
    }
    Ok(Session {
        renderer: renderer.clone(),
        _server: media_server,
    })
}

#[cfg(test)]
mod tests;

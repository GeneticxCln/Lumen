//! The one way subtitle code talks to the network: through the private-address guard, to hosts
//! the provider owns, with a size cap on what it reads.

use std::time::Duration;

use serde::de::DeserializeOwned;
use url::Url;

use crate::error::{PlayerError, Result, redacted};
use crate::netguard;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
/// Search answers are small; a bigger one is refused rather than buffered.
pub const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;

/// Which hosts a provider may talk to. A host matches when it is one of the suffixes or a
/// subdomain of one (`subs5.strem.io` for `strem.io`).
#[derive(Debug, Clone)]
pub struct Guard {
    suffixes: Vec<String>,
    allow_http: bool,
}

impl Guard {
    pub fn new(suffixes: &[&str]) -> Self {
        Self {
            suffixes: suffixes.iter().map(|s| s.to_ascii_lowercase()).collect(),
            allow_http: false,
        }
    }

    /// Any host over https. Only for addresses an extractor reported for a page the user chose;
    /// the private-address guard on the client still applies.
    pub fn any_public_host() -> Self {
        Self {
            suffixes: Vec::new(),
            allow_http: false,
        }
    }

    /// Plain http, for the in-process servers the tests start.
    #[cfg(test)]
    pub fn insecure_for_tests(suffixes: &[&str]) -> Self {
        Self {
            allow_http: true,
            ..Self::new(suffixes)
        }
    }

    pub fn check(&self, url: &Url) -> Result<()> {
        let scheme_ok = url.scheme() == "https" || (self.allow_http && url.scheme() == "http");
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        let host_ok = self.suffixes.is_empty()
            || self
                .suffixes
                .iter()
                .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")));
        if scheme_ok && host_ok {
            Ok(())
        } else {
            Err(PlayerError::InvalidArg(
                "That subtitle address is not on the service's own servers".into(),
            ))
        }
    }

    pub fn parse(&self, raw: &str) -> Result<Url> {
        let url = Url::parse(raw)
            .map_err(|_| PlayerError::InvalidArg("That is not a valid subtitle address".into()))?;
        self.check(&url)?;
        Ok(url)
    }
}

/// A client that refuses private addresses (unless the user allowed their network) and times out.
pub fn client(allow_lan: bool) -> Result<reqwest::Client> {
    netguard::guarded_client(allow_lan)
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("Lumen/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| PlayerError::Http(redacted(error)))
}

async fn get(client: &reqwest::Client, guard: &Guard, url: &Url) -> Result<reqwest::Response> {
    guard.check(url)?;
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|error| PlayerError::Http(redacted(error)))?;
    // A redirect may have left the provider's hosts; the guard applies to where it ended up too.
    guard.check(response.url())?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(PlayerError::NotFound(
            "the service has nothing for this".into(),
        ));
    }
    if !status.is_success() {
        return Err(PlayerError::Http(format!(
            "the service answered {}",
            status.as_u16()
        )));
    }
    Ok(response)
}

/// Reads a body, stopping with an error as soon as it passes `limit`.
pub async fn read_capped(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(PlayerError::Http(
            "the answer is larger than allowed".into(),
        ));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| PlayerError::Http(redacted(error)))?
    {
        if body.len() + chunk.len() > limit {
            return Err(PlayerError::Http(
                "the answer is larger than allowed".into(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub async fn get_json<T: DeserializeOwned>(
    client: &reqwest::Client,
    guard: &Guard,
    url: &Url,
) -> Result<T> {
    let body = read_capped(get(client, guard, url).await?, MAX_JSON_BYTES).await?;
    serde_json::from_slice(&body)
        .map_err(|error| PlayerError::Parse(format!("unexpected answer: {error}")))
}

pub async fn get_bytes(
    client: &reqwest::Client,
    guard: &Guard,
    url: &Url,
    limit: usize,
) -> Result<Vec<u8>> {
    read_capped(get(client, guard, url).await?, limit).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_providers_own_hosts_pass() {
        let guard = Guard::new(&["strem.io"]);
        for good in ["https://strem.io/a", "https://subs5.strem.io/en/download/x"] {
            assert!(guard.parse(good).is_ok(), "{good}");
        }
        for bad in [
            "http://subs5.strem.io/x",
            "https://evilstrem.io/x",
            "https://strem.io.evil.example/x",
            "https://example.com/x",
            "ftp://strem.io/x",
            "not a url",
        ] {
            assert!(guard.parse(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn oversize_bodies_and_redirects_off_the_provider_are_refused() {
        use crate::subtitles::testing::{Canned, serve};
        let server = serve(vec![
            Canned::bytes("/big", vec![b'x'; 2048]),
            Canned::bytes("/small", vec![b'x'; 16]),
            Canned::redirect("/away", "http://localhost:9/elsewhere"),
            Canned::redirect("/within", "__BASE__/small"),
            Canned::status("/gone", 404),
        ])
        .await;
        let guard = Guard::insecure_for_tests(&["127.0.0.1"]);
        let client = client(true).unwrap();
        let url = |path: &str| Url::parse(&format!("{}{path}", server.base)).unwrap();

        assert_eq!(
            get_bytes(&client, &guard, &url("/small"), 1024)
                .await
                .unwrap()
                .len(),
            16
        );
        assert!(
            get_bytes(&client, &guard, &url("/big"), 1024)
                .await
                .is_err(),
            "over the limit"
        );
        assert!(
            get_bytes(&client, &guard, &url("/away"), 1024)
                .await
                .is_err(),
            "left the provider's hosts"
        );
        assert_eq!(
            get_bytes(&client, &guard, &url("/within"), 1024)
                .await
                .unwrap()
                .len(),
            16
        );
        assert!(matches!(
            get_bytes(&client, &guard, &url("/gone"), 1024).await,
            Err(PlayerError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn a_redirect_to_a_private_address_is_refused() {
        use crate::subtitles::testing::{Canned, serve};
        // A hostile answer that points a download at a router or local service is stopped by the
        // redirect policy, whatever the provider's host list says.
        let server = serve(vec![Canned::redirect(
            "/bounce",
            "http://127.0.0.1:9/admin",
        )])
        .await;
        let guard = Guard::insecure_for_tests(&["127.0.0.1"]);
        let url = Url::parse(&format!("{}/bounce", server.base)).unwrap();
        // The first hop (a loopback IP literal here) is reached; the bounce to another private
        // address is what the redirect policy stops.
        let strict = client(false).unwrap();
        assert!(get_bytes(&strict, &guard, &url, 1024).await.is_err());
        assert_eq!(server.hits(), 1, "only the first hop was contacted");
    }
}

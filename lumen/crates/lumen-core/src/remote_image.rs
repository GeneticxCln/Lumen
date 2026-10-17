//! Fetches an image (a TMDB poster/backdrop, or a playlist's `tvg-logo`) on the frontend's
//! behalf, through the same guarded, network-privacy-aware client as everything else
//! (`netguard::guarded_client`), so the embedded webview never has to make the request itself.
//!
//! This only matters once network privacy is turned on: the frontend keeps loading these images
//! directly, the fast and normal way, until then (see the Tauri command's doc comment). When it
//! does call this, every existing protection already applies for free — private-address and
//! redirect checks, and `netguard::guarded_client`'s own proxy routing, including its fail-closed
//! behavior if the relay is meant to be on but is not usable.

use crate::error::{PlayerError, Result};
use crate::netguard;
use std::time::Duration;

/// Generous for a poster, backdrop or channel logo; anything bigger is refused rather than
/// buffered.
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

const IMAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub struct FetchedImage {
    /// The response's own `Content-Type`, e.g. `image/jpeg`, with no `; charset=...` suffix.
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// Fetches `url` and returns its bytes, refusing anything that is not declared as an image or
/// that is over [`MAX_IMAGE_BYTES`].
pub async fn fetch_image(url: &str, allow_lan: bool) -> Result<FetchedImage> {
    let validated = netguard::validate_url(url, allow_lan)?;
    let client = netguard::guarded_client(allow_lan)?
        .timeout(IMAGE_FETCH_TIMEOUT)
        .build()
        .map_err(|e| PlayerError::Http(format!("Could not set up the image client: {e}")))?;
    let response = client.get(validated).send().await?;
    if !response.status().is_success() {
        return Err(PlayerError::Http(format!(
            "The image server returned {}",
            response.status()
        )));
    }
    let content_type = content_type_of(&response);
    if !content_type.starts_with("image/") {
        return Err(PlayerError::Http(
            "That address did not return an image".into(),
        ));
    }
    let bytes = read_capped(response, MAX_IMAGE_BYTES).await?;
    Ok(FetchedImage {
        content_type,
        bytes,
    })
}

fn content_type_of(response: &reqwest::Response) -> String {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// Reads `response`'s body, refusing it as soon as its size passes `limit` (whether declared
/// upfront or found out while streaming), instead of buffering an arbitrarily large reply.
async fn read_capped(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if let Some(len) = response.content_length()
        && len > limit as u64
    {
        return Err(PlayerError::Http(format!(
            "That image is {len} bytes, over the {limit} byte limit"
        )));
    }
    let mut buffer: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if buffer.len() + chunk.len() > limit {
            return Err(PlayerError::Http(format!(
                "That image is over the {limit} byte limit"
            )));
        }
        buffer.extend_from_slice(&chunk);
    }
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitles::testing::{Canned, serve};

    #[tokio::test]
    async fn an_image_within_the_limit_is_returned_with_its_content_type() {
        let server = serve(vec![Canned::typed(
            "/logo.png",
            "image/png",
            vec![1, 2, 3, 4],
        )])
        .await;
        let image = fetch_image(&format!("{}/logo.png", server.base), true)
            .await
            .unwrap();
        assert_eq!(image.content_type, "image/png");
        assert_eq!(image.bytes, vec![1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn a_content_type_with_a_charset_suffix_is_still_recognized() {
        let server = serve(vec![Canned::typed(
            "/pic.svg",
            "image/svg+xml; charset=utf-8",
            b"<svg/>".to_vec(),
        )])
        .await;
        let image = fetch_image(&format!("{}/pic.svg", server.base), true)
            .await
            .unwrap();
        assert_eq!(image.content_type, "image/svg+xml");
    }

    #[tokio::test]
    async fn a_non_image_response_is_refused() {
        let server = serve(vec![Canned::json("/not-an-image", "{}")]).await;
        let error = fetch_image(&format!("{}/not-an-image", server.base), true)
            .await
            .unwrap_err();
        assert!(String::from(error).contains("did not return an image"));
    }

    #[tokio::test]
    async fn an_oversized_image_is_refused_rather_than_buffered() {
        let server = serve(vec![Canned::typed(
            "/huge.jpg",
            "image/jpeg",
            vec![0u8; MAX_IMAGE_BYTES + 1],
        )])
        .await;
        let error = fetch_image(&format!("{}/huge.jpg", server.base), true)
            .await
            .unwrap_err();
        assert!(String::from(error).contains("byte limit"));
    }

    #[tokio::test]
    async fn a_failing_status_is_refused() {
        let server = serve(vec![Canned::status("/missing.jpg", 404)]).await;
        let error = fetch_image(&format!("{}/missing.jpg", server.base), true)
            .await
            .unwrap_err();
        assert!(String::from(error).contains("404"));
    }

    #[tokio::test]
    async fn a_private_address_is_refused_without_the_local_network_switch() {
        let error = fetch_image("http://127.0.0.1:1/x.jpg", false)
            .await
            .unwrap_err();
        assert!(!String::from(error).is_empty());
    }
}

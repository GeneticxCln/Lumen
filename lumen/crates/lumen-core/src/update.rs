//! Updating Lumen from its own GitHub releases: the latest release is the update.
//!
//! Lumen asks GitHub for the newest published release of its repository, compares the tag with
//! the version it is running, and for an AppImage downloads the matching file, checks it against
//! the release's `SHA256SUMS` and puts it in place of the running file. Packages managed by a
//! system (a `.deb`, a Flatpak) are never replaced here; they only learn that a newer version
//! exists.
//!
//! The checksum alone catches a damaged or truncated download; it does not prove who published
//! the file, since the checksums come from the same place as the files. `update_sig` closes that
//! gap: `SHA256SUMS` must also carry a valid signature from the release key baked into Lumen
//! before anything it lists is trusted, which only whoever holds the matching secret key can
//! produce.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use url::Url;

use crate::error::{PlayerError, Result, redacted};
use crate::subtitles::http::Guard;

/// Where releases are listed. The tag of the newest published (not draft, not pre-release) one
/// is the version to be on.
pub const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/GeneticxCln/Lumen/releases/latest";

/// The repository's download path; release files are only taken from here.
const DOWNLOAD_PATH_PREFIX: &str = "/GeneticxCln/Lumen/releases/download/";

/// The checksum file every release carries.
pub const CHECKSUM_ASSET: &str = "SHA256SUMS";
/// The signature over [`CHECKSUM_ASSET`] every release carries, by the key in `update_sig`.
pub const CHECKSUM_SIG_ASSET: &str = "SHA256SUMS.minisig";

const MAX_METADATA_BYTES: usize = 2 * 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 800 * 1024 * 1024;

/// The hosts a release and its files are served from.
pub fn github_guard() -> Guard {
    Guard::new(&[
        "api.github.com",
        "github.com",
        "objects.githubusercontent.com",
        "release-assets.githubusercontent.com",
    ])
}

/// A version as `major.minor.patch`, with whether it is a pre-release (`-rc.1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    /// `false` sorts before `true`, so a pre-release is older than its release.
    release: bool,
}

impl Version {
    /// `v1.2.3`, `1.2.3` or `1.2.3-rc.1`; anything else is not a version.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_start_matches('v');
        let text = text.split('+').next().unwrap_or(text);
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, !pre.is_empty()),
            None => (text, false),
        };
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        parts.next().is_none().then_some(Self {
            major,
            minor,
            patch,
            release: !pre,
        })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.release
    }
}

/// The package a running copy of Lumen came from, which decides how it can be updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    /// A single file Lumen can replace.
    AppImage,
    /// Installed by a package manager or sandbox: Lumen only reports a newer version.
    Managed,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

/// A newer release, with what is needed to install it when that is possible.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Available {
    /// The release tag as published (`v0.2.0`).
    pub tag: String,
    pub page: String,
    /// The AppImage to download, if this release has one for this machine.
    pub package: Option<Package>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Package {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// Lowercase hex SHA-256 from the release's `SHA256SUMS`.
    pub sha256: String,
}

/// The value `uname -m` style names are written as in package file names.
pub fn arch_names(arch: &str) -> &'static [&'static str] {
    match arch {
        "x86_64" => &["amd64", "x86_64"],
        "aarch64" => &["aarch64", "arm64"],
        _ => &[],
    }
}

/// `name  sha256` lines as `sha256sum` writes them (`<hex>  <file>` or `<hex> *<file>`).
pub fn parse_checksums(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let (hash, name) = line.trim().split_once(char::is_whitespace)?;
            let name = name.trim().trim_start_matches('*');
            let valid = hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit());
            (valid && !name.is_empty()).then(|| (name.to_string(), hash.to_ascii_lowercase()))
        })
        .collect()
}

fn asset_url(guard: &Guard, asset: &ApiAsset) -> Result<Url> {
    let url = guard.parse(&asset.browser_download_url)?;
    if !url.path().starts_with(DOWNLOAD_PATH_PREFIX) {
        return Err(PlayerError::InvalidArg(
            "A release file is not from the Lumen repository".into(),
        ));
    }
    Ok(url)
}

fn client() -> Result<reqwest::Client> {
    // Public hosts only. Redirects are followed (release files redirect to a storage host) and
    // each hop is checked against the guard by `fetch`.
    crate::subtitles::http::client(false)
}

async fn fetch(client: &reqwest::Client, guard: &Guard, url: &Url) -> Result<reqwest::Response> {
    guard.check(url)?;
    let response = client
        .get(url.clone())
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| PlayerError::Http(redacted(e)))?;
    guard.check(response.url())?;
    if !response.status().is_success() {
        return Err(PlayerError::Http(format!(
            "GitHub answered {}",
            response.status().as_u16()
        )));
    }
    Ok(response)
}

/// Asks the release API (`api_url`) whether a release newer than `current` exists, and what to
/// download for `arch`. `want_package` is false for managed installs, which only need the news.
pub async fn check(
    guard: &Guard,
    api_url: &str,
    current: &str,
    arch: &str,
    want_package: bool,
) -> Result<Option<Available>> {
    check_using(&client()?, guard, api_url, current, arch, want_package).await
}

async fn check_using(
    client: &reqwest::Client,
    guard: &Guard,
    api_url: &str,
    current: &str,
    arch: &str,
    want_package: bool,
) -> Result<Option<Available>> {
    let current = Version::parse(current)
        .ok_or_else(|| PlayerError::Parse("This version of Lumen has no version number".into()))?;
    let api = guard.parse(api_url)?;
    let body =
        crate::subtitles::http::read_capped(fetch(client, guard, &api).await?, MAX_METADATA_BYTES)
            .await?;
    let release: ApiRelease = serde_json::from_slice(&body)
        .map_err(|e| PlayerError::Parse(format!("unexpected answer from GitHub: {e}")))?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let Some(latest) = Version::parse(&release.tag_name).filter(|v| !v.is_prerelease()) else {
        return Ok(None);
    };
    if latest <= current {
        return Ok(None);
    }
    let page = release.html_url.clone();
    if !want_package {
        return Ok(Some(Available {
            tag: release.tag_name,
            page,
            package: None,
        }));
    }
    let wanted = arch_names(arch);
    let asset = release.assets.iter().find(|asset| {
        asset.name.ends_with(".AppImage") && wanted.iter().any(|a| asset.name.contains(a))
    });
    let checksums = release.assets.iter().find(|a| a.name == CHECKSUM_ASSET);
    let package = match (asset, checksums) {
        (Some(asset), Some(checksums)) => {
            let sums = crate::subtitles::http::read_capped(
                fetch(client, guard, &asset_url(guard, checksums)?).await?,
                MAX_METADATA_BYTES,
            )
            .await?;
            // The checksum alone only proves the download was not corrupted; it says nothing
            // about who published it. A release with no signature asset, or one that does not
            // verify, is rejected outright here rather than falling back to checksum-only trust.
            let signature_asset = release
                .assets
                .iter()
                .find(|a| a.name == CHECKSUM_SIG_ASSET)
                .ok_or_else(|| {
                    PlayerError::Parse("This release has no SHA256SUMS.minisig".into())
                })?;
            let signature = crate::subtitles::http::read_capped(
                fetch(client, guard, &asset_url(guard, signature_asset)?).await?,
                MAX_METADATA_BYTES,
            )
            .await?;
            crate::update_sig::verify_checksums_signature(
                &sums,
                &String::from_utf8_lossy(&signature),
            )?;
            let sums = parse_checksums(&String::from_utf8_lossy(&sums));
            match sums.get(&asset.name) {
                Some(sha256) => Some(Package {
                    name: asset.name.clone(),
                    url: asset_url(guard, asset)?.to_string(),
                    size: asset.size,
                    sha256: sha256.clone(),
                }),
                None => None,
            }
        }
        _ => None,
    };
    Ok(Some(Available {
        tag: release.tag_name,
        page,
        package,
    }))
}

/// Downloads `package` into `directory`, checks its SHA-256, and returns the finished file. A
/// download that is too large, too short or does not match is deleted and refused.
pub async fn download(guard: &Guard, package: &Package, directory: &Path) -> Result<PathBuf> {
    download_using(&client()?, guard, package, directory).await
}

async fn download_using(
    client: &reqwest::Client,
    guard: &Guard,
    package: &Package,
    directory: &Path,
) -> Result<PathBuf> {
    let url = guard.parse(&package.url)?;
    let name = Path::new(&package.name)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.starts_with('.') && *n == package.name)
        .ok_or_else(|| PlayerError::InvalidArg("That is not a usable file name".into()))?;
    let limit = if package.size > 0 {
        package.size.min(MAX_PACKAGE_BYTES)
    } else {
        MAX_PACKAGE_BYTES
    };
    let partial = directory.join(format!(".{name}.part"));
    let outcome = write_verified(client, guard, &url, &partial, limit, &package.sha256).await;
    if outcome.is_err() {
        let _ = tokio::fs::remove_file(&partial).await;
    }
    outcome?;
    let finished = directory.join(name);
    tokio::fs::rename(&partial, &finished).await?;
    Ok(finished)
}

async fn write_verified(
    client: &reqwest::Client,
    guard: &Guard,
    url: &Url,
    path: &Path,
    limit: u64,
    expected: &str,
) -> Result<()> {
    let mut response = fetch(client, guard, url).await?;
    if response.content_length().is_some_and(|n| n > limit) {
        return Err(PlayerError::Http(
            "The download is larger than expected".into(),
        ));
    }
    let mut file = tokio::fs::File::create(path).await?;
    let mut hasher = Sha256::new();
    let mut written = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| PlayerError::Http(redacted(e)))?
    {
        written += chunk.len() as u64;
        if written > limit {
            return Err(PlayerError::Http(
                "The download is larger than expected".into(),
            ));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(PlayerError::Parse(
            "The download does not match the published checksum, so it was discarded".into(),
        ));
    }
    Ok(())
}

/// Puts the downloaded AppImage in place of `target` (the running one). Both are on the same
/// filesystem (the download goes next to the target), so the swap is a single rename: the running
/// copy keeps working from the old file until it is restarted.
pub fn replace_appimage(downloaded: &Path, target: &Path) -> Result<()> {
    let mut magic = [0u8; 4];
    {
        use std::io::Read;
        std::fs::File::open(downloaded)?.read_exact(&mut magic)?;
    }
    if &magic != b"\x7fELF" {
        return Err(PlayerError::Parse(
            "The downloaded file is not a program".into(),
        ));
    }
    let metadata = std::fs::metadata(target)?;
    if !metadata.is_file() {
        return Err(PlayerError::InvalidArg(
            "The running copy is not a file".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            downloaded,
            std::fs::Permissions::from_mode(metadata.permissions().mode() | 0o111),
        )?;
    }
    std::fs::rename(downloaded, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitles::testing::{Canned, serve};

    #[test]
    fn versions_compare_by_number_not_by_text() {
        let v = |t: &str| Version::parse(t).unwrap();
        assert!(v("v0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("1.2.3") == v("v1.2.3"));
        assert!(
            v("1.2.3-rc.1") < v("1.2.3"),
            "a release candidate is older than its release"
        );
        assert!(v("1.2.3-rc.1").is_prerelease() && !v("1.2.3").is_prerelease());
        assert!(v("1.2.3+build5") == v("1.2.3"));
        for bad in ["", "1", "1.2", "1.2.3.4", "latest", "v1.x.0", "1.2.-3"] {
            assert!(Version::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn checksum_files_are_read_the_way_sha256sum_writes_them() {
        let hash_a = "a".repeat(64);
        let hash_b = "B".repeat(64);
        let text = format!(
            "{hash_a}  lumen_0.2.0_amd64.AppImage\n{hash_b} *lumen_0.2.0_amd64.deb\nnot a line\n{} short.bin\n",
            "c".repeat(10)
        );
        let sums = parse_checksums(&text);
        assert_eq!(sums.len(), 2);
        assert_eq!(sums["lumen_0.2.0_amd64.AppImage"], hash_a);
        assert_eq!(sums["lumen_0.2.0_amd64.deb"], "b".repeat(64), "lowercased");
    }

    fn sha(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// A release API answer for a server whose address is `__BASE__` (filled in by the server).
    /// Includes a `SHA256SUMS.minisig` asset so tests that reach the checksum-verification
    /// branch also need to serve a matching signature at `sig_path(tag)` — see
    /// `a_release_with_no_signature_asset_is_refused` for the one case that deliberately omits it.
    fn release_json(tag: &str, extra: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","html_url":"__BASE__/GeneticxCln/Lumen/releases/tag/{tag}"{extra},"assets":[
                {{"name":"lumen_0.2.0_amd64.deb","browser_download_url":"__BASE__/GeneticxCln/Lumen/releases/download/{tag}/lumen_0.2.0_amd64.deb","size":10}},
                {{"name":"lumen_0.2.0_amd64.AppImage","browser_download_url":"__BASE__/GeneticxCln/Lumen/releases/download/{tag}/lumen_0.2.0_amd64.AppImage","size":2048}},
                {{"name":"SHA256SUMS","browser_download_url":"__BASE__/GeneticxCln/Lumen/releases/download/{tag}/SHA256SUMS","size":200}},
                {{"name":"SHA256SUMS.minisig","browser_download_url":"__BASE__/GeneticxCln/Lumen/releases/download/{tag}/SHA256SUMS.minisig","size":200}}]}}"#
        )
    }

    /// A release JSON with no `SHA256SUMS.minisig` asset at all.
    fn release_json_without_signature_asset(tag: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","html_url":"__BASE__/GeneticxCln/Lumen/releases/tag/{tag}","assets":[
                {{"name":"lumen_0.2.0_amd64.AppImage","browser_download_url":"__BASE__/GeneticxCln/Lumen/releases/download/{tag}/lumen_0.2.0_amd64.AppImage","size":2048}},
                {{"name":"SHA256SUMS","browser_download_url":"__BASE__/GeneticxCln/Lumen/releases/download/{tag}/SHA256SUMS","size":200}}]}}"#
        )
    }

    fn sig_path(tag: &str) -> String {
        format!("/GeneticxCln/Lumen/releases/download/{tag}/SHA256SUMS.minisig")
    }

    /// A valid signature (by the `cfg(test)` key in `update_sig`) over `lumen_0.2.0_amd64.AppImage`'s
    /// checksum (computed from the padded `[0x7f, b'E', b'L', b'F', 1, 2, 3]` content used below)
    /// plus a throwaway `lumen_0.2.0_amd64.deb` line of all zeros — the exact `SHA256SUMS` content
    /// `a_newer_release_is_found_with_its_checked_package` serves.
    const SIG_FOR_HAPPY_PATH_SUMS: &str = "untrusted comment: signature from minisign secret key
RUSXv6TLERTKxO01YE6unRN8ngK/4EDik9CH8ijeJK6NTNeaSQK9K1Z0rj+6sxgPAWJTEmE5HJe8kELYyl1P/Ny6MES6o2goyQI=
trusted comment: timestamp:1792861956\tfile:sums1.txt\thashed
bRsnjgwjySp70nfziaqAhB67t/M3UolEjPRFbJqt3PbK/rXlHT3Et53wA3Cp18lNRnNmwtTxItM7t2Cl+zb8Dw==";

    /// A valid signature over `a_release_without_a_checksum_for_the_file_offers_no_package`'s
    /// `others` fixture content (`"1".repeat(64)  something-else.AppImage\n`).
    const SIG_FOR_UNMATCHED_SUMS: &str = "untrusted comment: signature from minisign secret key
RUSXv6TLERTKxIdxILq5+fuSfi7oDb1jYBuE8wTGYJsxiGC67C7y8teFPmP5hoWfX0/uktsaHMOXwRQgEkuF2EwEoBnRcwiT7AE=
trusted comment: timestamp:1792861959\tfile:sums3.txt\thashed
R24flS/937jhO5jL/vUpS3QVtehPv8PSYaTyrd4lY+l63uXCJWm91WhoojODm8s22Cz971HUEmP22pMHtuWEAA==";

    /// A valid signature over `files_from_outside_the_repository_are_refused`'s fixture content
    /// (`"1".repeat(64)  lumen_0.2.0_amd64.AppImage\n`).
    const SIG_FOR_ESCAPED_ASSET_SUMS: &str = "untrusted comment: signature from minisign secret key
RUSXv6TLERTKxB3tL9CSR0WsMZLvKuKzRNx/NEk3zW5A6t2DNojwLi6qxDcajQPUfHliZIU5tOv5u8xKDmgtNpy9jAuCJs1RFwE=
trusted comment: timestamp:1792861962\tfile:sums4.txt\thashed
ld6q8vSqr6xzIo6v/nzj3y7EvprIjNzNIQT/KLUvJepFVT6s3gPH83lBzEuNyFp+YoXjLMnNdK/Z1GKqijKiBw==";

    fn guard() -> Guard {
        Guard::insecure_for_tests(&["127.0.0.1"])
    }

    // The tests talk to a server on 127.0.0.1, which the production client refuses; they use a
    // client that allows the local network.
    fn local_client() -> reqwest::Client {
        crate::subtitles::http::client(true).unwrap()
    }

    async fn check_local(
        base: &str,
        current: &str,
        want_package: bool,
    ) -> Result<Option<Available>> {
        check_using(
            &local_client(),
            &guard(),
            &format!("{base}/api"),
            current,
            "x86_64",
            want_package,
        )
        .await
    }

    #[tokio::test]
    async fn a_newer_release_is_found_with_its_checked_package() {
        let package = vec![0x7f, b'E', b'L', b'F', 1, 2, 3];
        let mut content = package.clone();
        content.resize(2048, 9);
        let sums = format!(
            "{}  lumen_0.2.0_amd64.AppImage\n{}  lumen_0.2.0_amd64.deb\n",
            sha(&content),
            "0".repeat(64)
        );
        let server = serve(vec![
            Canned::json("/api", &release_json("v0.2.0", "")),
            Canned::text(
                "/GeneticxCln/Lumen/releases/download/v0.2.0/SHA256SUMS",
                &sums,
            ),
            Canned::text(&sig_path("v0.2.0"), SIG_FOR_HAPPY_PATH_SUMS),
        ])
        .await;
        let found = check_local(&server.base, "0.1.0", true)
            .await
            .unwrap()
            .expect("newer");
        assert_eq!(found.tag, "v0.2.0");
        let package = found.package.expect("a package for this machine");
        assert_eq!(package.name, "lumen_0.2.0_amd64.AppImage");
        assert_eq!(package.sha256, sha(&content));
    }

    const SUMS_PATH: &str = "/GeneticxCln/Lumen/releases/download/v0.2.0/SHA256SUMS";

    #[tokio::test]
    async fn nothing_is_offered_unless_the_release_is_newer_and_final() {
        let server = serve(vec![Canned::json("/api", &release_json("v0.2.0", ""))]).await;
        assert!(
            check_local(&server.base, "0.2.0", true)
                .await
                .unwrap()
                .is_none(),
            "same version"
        );
        assert!(
            check_local(&server.base, "0.3.1", true)
                .await
                .unwrap()
                .is_none(),
            "running a newer one"
        );
        assert!(
            check_local(&server.base, "0.1.9", false)
                .await
                .unwrap()
                .is_some()
        );

        let draft = serve(vec![Canned::json(
            "/api",
            &release_json("v0.2.0", r#","draft":true"#),
        )])
        .await;
        assert!(
            check_local(&draft.base, "0.1.0", true)
                .await
                .unwrap()
                .is_none()
        );
        let pre = serve(vec![Canned::json(
            "/api",
            &release_json("v0.2.0", r#","prerelease":true"#),
        )])
        .await;
        assert!(
            check_local(&pre.base, "0.1.0", true)
                .await
                .unwrap()
                .is_none()
        );
        let rc = serve(vec![Canned::json("/api", &release_json("v0.2.0-rc.1", ""))]).await;
        assert!(
            check_local(&rc.base, "0.1.0", true)
                .await
                .unwrap()
                .is_none(),
            "a tag that is a pre-release"
        );
        let junk = serve(vec![Canned::json("/api", &release_json("nightly", ""))]).await;
        assert!(
            check_local(&junk.base, "0.1.0", true)
                .await
                .unwrap()
                .is_none(),
            "a tag that is not a version"
        );
    }

    #[tokio::test]
    async fn a_release_without_a_checksum_for_the_file_offers_no_package() {
        let others = format!("{}  something-else.AppImage\n", "1".repeat(64));
        let server = serve(vec![
            Canned::json("/api", &release_json("v0.2.0", "")),
            Canned::text(SUMS_PATH, &others),
            Canned::text(&sig_path("v0.2.0"), SIG_FOR_UNMATCHED_SUMS),
        ])
        .await;
        let found = check_local(&server.base, "0.1.0", true)
            .await
            .unwrap()
            .unwrap();
        assert!(
            found.package.is_none(),
            "the news is reported but it is not installed unverified"
        );
    }

    #[tokio::test]
    async fn files_from_outside_the_repository_are_refused() {
        let json = release_json("v0.2.0", "").replace(
            "/GeneticxCln/Lumen/releases/download/v0.2.0/lumen_0.2.0_amd64.AppImage",
            "/someone/else/lumen.AppImage",
        );
        let sums = format!("{}  lumen_0.2.0_amd64.AppImage\n", "1".repeat(64));
        let server = serve(vec![
            Canned::json("/api", &json),
            Canned::text(SUMS_PATH, &sums),
            Canned::text(&sig_path("v0.2.0"), SIG_FOR_ESCAPED_ASSET_SUMS),
        ])
        .await;
        assert!(check_local(&server.base, "0.1.0", true).await.is_err());
    }

    #[tokio::test]
    async fn a_release_with_no_signature_asset_is_refused() {
        let sums = format!("{}  lumen_0.2.0_amd64.AppImage\n", "1".repeat(64));
        let server = serve(vec![
            Canned::json("/api", &release_json_without_signature_asset("v0.2.0")),
            Canned::text(SUMS_PATH, &sums),
        ])
        .await;
        assert!(
            check_local(&server.base, "0.1.0", true).await.is_err(),
            "checksums alone, with no signature to trust them, must not be enough"
        );
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lumen-update-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn package_for(server: &str, content: &[u8], sha256: String) -> Package {
        Package {
            name: "lumen_0.2.0_amd64.AppImage".into(),
            url: format!("{server}/file"),
            size: content.len() as u64,
            sha256,
        }
    }

    #[tokio::test]
    async fn a_download_is_kept_only_if_it_matches_its_checksum() {
        let content: Vec<u8> = b"\x7fELF"
            .iter()
            .copied()
            .chain((0..5000u32).map(|n| n as u8))
            .collect();
        let server = serve(vec![Canned::bytes("/file", content.clone())]).await;
        let dir = temp_dir();

        let good = package_for(&server.base, &content, sha(&content));
        let path = download_using(&local_client(), &guard(), &good, &dir)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), content);
        std::fs::remove_file(&path).unwrap();

        let wrong = package_for(&server.base, &content, "0".repeat(64));
        assert!(
            download_using(&local_client(), &guard(), &wrong, &dir)
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "nothing is left behind"
        );

        let mut small = package_for(&server.base, &content, sha(&content));
        small.size = 100;
        assert!(
            download_using(&local_client(), &guard(), &small, &dir)
                .await
                .is_err(),
            "larger than announced"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);

        let mut named = package_for(&server.base, &content, sha(&content));
        named.name = "../escape.AppImage".into();
        assert!(
            download_using(&local_client(), &guard(), &named, &dir)
                .await
                .is_err(),
            "a name cannot leave the folder"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_download_cannot_be_redirected_off_github() {
        let server = serve(vec![Canned::redirect(
            "/file",
            "http://localhost:9/elsewhere",
        )])
        .await;
        let dir = temp_dir();
        let package = package_for(&server.base, b"x", sha(b"x"));
        assert!(
            download_using(&local_client(), &guard(), &package, &dir)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_running_file_is_swapped_for_a_program_only() {
        let dir = temp_dir();
        let target = dir.join("Lumen.AppImage");
        std::fs::write(&target, b"old").unwrap();
        let fresh = dir.join("new.AppImage");
        std::fs::write(&fresh, b"\x7fELF-new").unwrap();
        replace_appimage(&fresh, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"\x7fELF-new");
        assert!(!fresh.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o111 != 0,
                "it can run"
            );
        }

        let html = dir.join("page.html");
        std::fs::write(&html, b"<html>error</html>").unwrap();
        assert!(
            replace_appimage(&html, &target).is_err(),
            "a web page is not a program"
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"\x7fELF-new",
            "the running file is untouched"
        );
        assert!(replace_appimage(&fresh, &dir.join("missing")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn package_names_are_matched_by_architecture() {
        assert!(arch_names("x86_64").contains(&"amd64"));
        assert!(arch_names("aarch64").contains(&"arm64"));
        assert!(
            arch_names("riscv64").is_empty(),
            "an unknown machine matches nothing"
        );
    }
}

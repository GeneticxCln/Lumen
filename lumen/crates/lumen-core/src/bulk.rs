//! Adding many playlists at once: finding the links in a text, and testing each one before it is
//! added. Nothing here adds anything; the caller adds what the user chooses.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::Semaphore;

use crate::db;
use crate::playlist::import::{fetch_playlist_text, import_m3u_url};
use crate::playlist::m3u::{Channel, ParsedPlaylist, is_hls_manifest, parse_m3u};
use crate::probe::{ProbeTarget, probe_targets_bounded};

/// The most links taken from one text.
pub const MAX_LINKS: usize = 500;
/// How many links are tested at the same time.
const CONCURRENT_CHECKS: usize = 4;
/// How long one link gets, fetch and sample together.
const CHECK_TIMEOUT: Duration = Duration::from_secs(25);
/// How many links are added at the same time.
const CONCURRENT_ADDS: usize = 4;
/// How long one link gets to be fetched and imported before it is treated as failed. A slow or
/// unresponsive host must not turn one click into an hours-long, silent wait that looks exactly
/// like a hang because nothing tells the page it is still working.
const ADD_TIMEOUT: Duration = Duration::from_secs(35);
/// Channels of a playlist that are tried to see whether it points at anything alive.
const SAMPLE_SIZE: usize = 5;
const SAMPLE_DEADLINE: Duration = Duration::from_secs(8);

/// Every `http(s)` link in `text`, in order and without repeats, at most `MAX_LINKS`. Whatever is
/// around a link (prose, Markdown, a CSV cell, quotes, a closing bracket) is not part of it.
pub fn extract_links(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut links = Vec::new();
    let mut rest = text;
    while let Some(start) = find_scheme(rest) {
        let tail = &rest[start..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`' | '\\'))
            .unwrap_or(tail.len());
        let link = trim_link(&tail[..end]);
        rest = &tail[end.max(1)..];
        // `http://` alone is not a link.
        let after = link.split_once("://").map(|(_, r)| r).unwrap_or_default();
        if after.is_empty() || !seen.insert(link.to_string()) {
            continue;
        }
        links.push(link.to_string());
        if links.len() == MAX_LINKS {
            break;
        }
    }
    links
}

fn find_scheme(text: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    match (lower.find("http://"), lower.find("https://")) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Removes punctuation that belongs to the sentence, not the link; a closing bracket stays when
/// the link has its own opening one.
fn trim_link(link: &str) -> &str {
    let mut link = link;
    loop {
        let Some(last) = link.chars().last() else {
            return link;
        };
        let strip = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '*' | '}' | ']' | '>' => true,
            ')' => link.matches(')').count() > link.matches('(').count(),
            _ => false,
        };
        if !strip {
            return link;
        }
        link = &link[..link.len() - last.len_utf8()];
    }
}

/// What testing one link found.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Outcome {
    /// A channel list. `responding` of the `sampled` channels tried answered.
    #[serde(rename_all = "camelCase")]
    Playlist {
        name: String,
        channels: usize,
        sampled: usize,
        responding: usize,
    },
    /// A single stream (an HLS manifest): one entry, played as it is.
    Stream {
        name: String,
    },
    Failed {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LinkCheck {
    pub url: String,
    pub outcome: Outcome,
}

impl LinkCheck {
    /// Whether the link can be added.
    pub fn usable(&self) -> bool {
        !matches!(self.outcome, Outcome::Failed { .. })
    }
}

fn failed(url: &str, reason: impl Into<String>) -> LinkCheck {
    LinkCheck {
        url: url.to_string(),
        outcome: Outcome::Failed {
            reason: reason.into(),
        },
    }
}

/// A name for a playlist from its address: the last part of the path without its extension.
pub fn name_from_link(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let rest = path.split_once("://").map(|(_, rest)| rest).unwrap_or(path);
    let mut parts = rest.trim_end_matches('/').split('/');
    let host = parts.next().unwrap_or("Playlist");
    match parts.next_back().filter(|segment| !segment.is_empty()) {
        Some(file) => file
            .rsplit_once('.')
            .map(|(stem, _)| stem)
            .filter(|stem| !stem.is_empty())
            .unwrap_or(file)
            .to_string(),
        None => host.to_string(),
    }
}

/// Whether `text` is a playlist at all: a web page or an error message is not.
fn looks_like_playlist(text: &str) -> bool {
    let head = text.trim_start_matches('\u{feff}').trim_start();
    head.starts_with("#EXTM3U") || text.contains("#EXTINF:") || is_hls_manifest(text)
}

fn sample(channels: &[Channel]) -> Vec<ProbeTarget> {
    let candidates: Vec<&Channel> = channels
        .iter()
        .filter(|c| c.url.starts_with("http://") || c.url.starts_with("https://"))
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    // Spread over the list, so the first few entries do not decide for all of it.
    let wanted = candidates.len().min(SAMPLE_SIZE);
    (0..wanted)
        .map(|i| {
            let channel = candidates[i * candidates.len() / wanted];
            ProbeTarget {
                url: channel.url.clone(),
                headers: channel
                    .extra_headers
                    .as_deref()
                    .and_then(|json| serde_json::from_str(json).ok())
                    .unwrap_or_default(),
            }
        })
        .collect()
}

/// Tests one link: downloads it through the network guard, checks that it is a playlist, and tries
/// a few of its channels.
pub async fn check_link(url: &str, allow_lan: bool) -> LinkCheck {
    match tokio::time::timeout(CHECK_TIMEOUT, check_inner(url, allow_lan)).await {
        Ok(check) => check,
        Err(_) => failed(url, "took too long to answer"),
    }
}

async fn check_inner(url: &str, allow_lan: bool) -> LinkCheck {
    let text = match fetch_playlist_text(url, allow_lan).await {
        Ok(text) => text,
        Err(error) => return failed(url, String::from(error)),
    };
    if !looks_like_playlist(&text) {
        return failed(url, "not a playlist (no #EXTM3U or #EXTINF lines)");
    }
    let name = name_from_link(url);
    let parsed = {
        let (text, link) = (text.clone(), url.to_string());
        tokio::task::spawn_blocking(move || parse_m3u(&text, 0, Some(&link))).await
    };
    let channels = match parsed {
        Ok(Ok(ParsedPlaylist::Hls(_))) => {
            return LinkCheck {
                url: url.to_string(),
                outcome: Outcome::Stream { name },
            };
        }
        Ok(Ok(ParsedPlaylist::Channels(channels))) if !channels.is_empty() => channels,
        Ok(Ok(_)) => return failed(url, "the playlist has no channels"),
        Ok(Err(error)) => return failed(url, String::from(error)),
        Err(_) => return failed(url, "could not read the playlist"),
    };
    let targets = sample(&channels);
    let sampled = targets.len();
    let responding = if sampled == 0 {
        0
    } else {
        probe_targets_bounded(targets, allow_lan, SAMPLE_DEADLINE, |_, _| {})
            .await
            .iter()
            .flatten()
            .filter(|result| result.live)
            .count()
    };
    LinkCheck {
        url: url.to_string(),
        outcome: Outcome::Playlist {
            name,
            channels: channels.len(),
            sampled,
            responding,
        },
    }
}

/// Tests `urls` a few at a time and hands each result to `on_result` as soon as it is ready.
/// Setting `cancel` stops links that have not started yet. Returns how many were tested.
pub async fn check_links<F>(
    urls: Vec<String>,
    allow_lan: bool,
    cancel: Arc<AtomicBool>,
    on_result: F,
) -> usize
where
    F: Fn(LinkCheck) + Send + Sync + 'static,
{
    let gate = Arc::new(Semaphore::new(CONCURRENT_CHECKS));
    let on_result = Arc::new(on_result);
    let mut tasks = tokio::task::JoinSet::new();
    for url in urls {
        let (gate, cancel, on_result) = (gate.clone(), cancel.clone(), on_result.clone());
        tasks.spawn(async move {
            let Ok(_permit) = gate.acquire_owned().await else {
                return false;
            };
            if cancel.load(Ordering::SeqCst) {
                return false;
            }
            on_result(check_link(&url, allow_lan).await);
            true
        });
    }
    let mut tested = 0;
    while let Some(done) = tasks.join_next().await {
        if matches!(done, Ok(true)) {
            tested += 1;
        }
    }
    tested
}

/// What adding one link found.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AddOutcome {
    Added { playlist_id: i64 },
    Failed { reason: String },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AddResult {
    pub url: String,
    pub outcome: AddOutcome,
}

/// Adds each of `urls` as its own playlist, a few at a time — the same shape `check_links`
/// already tests links in — reporting each result to `on_result` as soon as it is ready instead
/// of leaving a whole batch to resolve silently behind one "Adding…". `known` is skipped before
/// anything starts; `cancel` stops links that have not started yet.
pub async fn add_links<F>(
    pool: &sqlx::Pool<sqlx::Sqlite>,
    urls: Vec<String>,
    allow_lan: bool,
    known: &HashSet<String>,
    cancel: Arc<AtomicBool>,
    on_result: F,
) -> Vec<AddResult>
where
    F: Fn(AddResult) + Send + Sync + 'static,
{
    let gate = Arc::new(Semaphore::new(CONCURRENT_ADDS));
    let on_result = Arc::new(on_result);
    let mut tasks = tokio::task::JoinSet::new();
    for url in urls {
        if known.contains(&url) {
            continue;
        }
        let (gate, cancel, pool, on_result) = (
            gate.clone(),
            cancel.clone(),
            pool.clone(),
            on_result.clone(),
        );
        tasks.spawn(async move {
            let Ok(_permit) = gate.acquire_owned().await else {
                return None;
            };
            if cancel.load(Ordering::SeqCst) {
                return None;
            }
            let name = Some(name_from_link(&url));
            let attempt = tokio::time::timeout(ADD_TIMEOUT, async {
                let (playlist, channels) = import_m3u_url(&url, name, allow_lan).await?;
                db::import_playlist(&pool, &playlist, &channels).await
            })
            .await;
            let outcome = match attempt {
                Ok(Ok(playlist_id)) => AddOutcome::Added { playlist_id },
                Ok(Err(error)) => AddOutcome::Failed {
                    reason: String::from(error),
                },
                Err(_) => AddOutcome::Failed {
                    reason: "took too long to answer".into(),
                },
            };
            let result = AddResult { url, outcome };
            on_result(result.clone());
            Some(result)
        });
    }
    let mut results = Vec::new();
    while let Some(done) = tasks.join_next().await {
        if let Ok(Some(result)) = done {
            results.push(result);
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitles::testing::{Canned, serve};
    use std::sync::Mutex;

    #[test]
    fn links_are_found_in_prose_markdown_and_tables() {
        let text = "My lists: https://a.example/one.m3u, and (see https://b.example/two.m3u8).\n\
            [three](https://c.example/three.m3u) <https://d.example/four.m3u>\n\
            name,url\nfive,\"https://e.example/five.m3u?token=1&x=2\"\n\
            Wikipedia style https://f.example/wiki/Foo_(bar) ok. ftp://g.example/x.m3u mailto:me@x.org\n\
            https://a.example/one.m3u again\nhttp://\nHTTPS://UPPER.example/X.M3U";
        assert_eq!(
            extract_links(text),
            [
                "https://a.example/one.m3u",
                "https://b.example/two.m3u8",
                "https://c.example/three.m3u",
                "https://d.example/four.m3u",
                "https://e.example/five.m3u?token=1&x=2",
                "https://f.example/wiki/Foo_(bar)",
                "HTTPS://UPPER.example/X.M3U",
            ]
        );
    }

    #[test]
    fn links_are_capped_and_empty_text_gives_none() {
        let many: String = (0..800)
            .map(|i| format!("https://x.example/{i}.m3u\n"))
            .collect();
        assert_eq!(extract_links(&many).len(), MAX_LINKS);
        assert!(extract_links("").is_empty());
        assert!(extract_links("no links, just words and ftp://x").is_empty());
    }

    #[test]
    fn names_come_from_the_address() {
        assert_eq!(
            name_from_link("https://x.example/lists/news.m3u8?t=1"),
            "news"
        );
        assert_eq!(name_from_link("https://x.example/lists/"), "lists");
        assert_eq!(name_from_link("https://x.example"), "x.example");
        assert_eq!(name_from_link("https://x.example/a.b.c.m3u"), "a.b.c");
    }

    const GOOD: &str = "#EXTM3U\n#EXTINF:-1 group-title=\"News\",One\n__BASE__/live/1.ts\n#EXTINF:-1,Two\n__BASE__/live/2.ts\n#EXTINF:-1,Dead\n__BASE__/gone/3.ts\n";
    const HLS: &str = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n#EXTINF:6.0,\nseg0.ts\n#EXT-X-ENDLIST\n";

    async fn fixture() -> crate::subtitles::testing::Server {
        serve(vec![
            Canned::text("/good.m3u", GOOD),
            Canned::text("/hls.m3u8", HLS),
            Canned::text(
                "/page.m3u",
                "<!doctype html><html><body>Sign in</body></html>",
            ),
            Canned::text("/empty.m3u", "#EXTM3U\n"),
            Canned::status("/missing.m3u", 404),
            Canned::redirect("/private.m3u", "http://169.254.169.254/latest"),
            Canned::bytes("/live/1.ts", vec![0; 16]),
            Canned::bytes("/live/2.ts", vec![0; 16]),
        ])
        .await
    }

    #[tokio::test]
    async fn a_good_playlist_is_counted_and_sampled() {
        let server = fixture().await;
        let check = check_link(&format!("{}/good.m3u", server.base), true).await;
        assert_eq!(
            check.outcome,
            Outcome::Playlist {
                name: "good".into(),
                channels: 3,
                sampled: 3,
                responding: 2
            },
            "two of the three channels answer, the third is gone"
        );
        assert!(check.usable());
    }

    #[tokio::test]
    async fn everything_that_is_not_a_playlist_fails_with_a_reason() {
        let server = fixture().await;
        let base = &server.base;
        let hls = check_link(&format!("{base}/hls.m3u8"), true).await;
        assert_eq!(hls.outcome, Outcome::Stream { name: "hls".into() });
        for (path, expected) in [
            ("/page.m3u", "not a playlist"),
            ("/empty.m3u", "no channels"),
            ("/missing.m3u", "404"),
            ("/private.m3u", ""),
            ("/nowhere.m3u", "404"),
        ] {
            let check = check_link(&format!("{base}{path}"), true).await;
            match check.outcome {
                Outcome::Failed { reason } => {
                    assert!(reason.contains(expected), "{path}: {reason}")
                }
                other => panic!("{path} should fail, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn the_local_network_is_refused_unless_allowed() {
        let server = fixture().await;
        let check = check_link(&format!("{}/good.m3u", server.base), false).await;
        match check.outcome {
            Outcome::Failed { reason } => assert!(
                reason.contains("local") || reason.contains("private"),
                "{reason}"
            ),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn links_are_checked_together_and_cancel_stops_the_rest() {
        let server = fixture().await;
        let urls: Vec<String> = ["good.m3u", "hls.m3u8", "page.m3u", "missing.m3u"]
            .iter()
            .map(|p| format!("{}/{p}", server.base))
            .collect();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let tested = check_links(
            urls.clone(),
            true,
            Arc::new(AtomicBool::new(false)),
            move |c| sink.lock().unwrap().push(c),
        )
        .await;
        assert_eq!(tested, 4);
        let results = seen.lock().unwrap().clone();
        assert_eq!(results.len(), 4);
        assert_eq!(results.iter().filter(|c| c.usable()).count(), 2);

        // Cancelled before it starts: nothing is tested.
        let none = check_links(urls, true, Arc::new(AtomicBool::new(true)), |_| {
            panic!("tested")
        })
        .await;
        assert_eq!(none, 0);
    }

    async fn test_pool() -> sqlx::Pool<sqlx::Sqlite> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn adding_is_concurrent_skips_known_links_and_reports_each_result() {
        let server = fixture().await;
        let pool = test_pool().await;
        let good = format!("{}/good.m3u", server.base);
        let missing = format!("{}/missing.m3u", server.base);
        let already_known = format!("{}/page.m3u", server.base);
        let urls = vec![good.clone(), missing.clone(), already_known.clone()];
        let known: HashSet<String> = [already_known].into_iter().collect();

        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let results = add_links(
            &pool,
            urls,
            true,
            &known,
            Arc::new(AtomicBool::new(false)),
            move |result| sink.lock().unwrap().push(result),
        )
        .await;

        assert_eq!(
            results.len(),
            2,
            "the already-known link is skipped before anything starts, not reported as failed"
        );
        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "every result returned was also reported live via on_result"
        );

        let added = results
            .iter()
            .find(|r| r.url == good)
            .expect("good.m3u result");
        match &added.outcome {
            AddOutcome::Added { playlist_id } => assert!(*playlist_id > 0),
            other => panic!("expected Added, got {other:?}"),
        }
        let failed = results
            .iter()
            .find(|r| r.url == missing)
            .expect("missing.m3u result");
        match &failed.outcome {
            AddOutcome::Failed { reason } => assert!(reason.contains("404"), "{reason}"),
            other => panic!("expected Failed, got {other:?}"),
        }

        let playlists = db::list_playlists(&pool).await.unwrap();
        assert_eq!(
            playlists.len(),
            1,
            "only the one successfully-imported link actually landed in the database"
        );
    }

    #[tokio::test]
    async fn cancelling_before_it_starts_adds_nothing() {
        let server = fixture().await;
        let pool = test_pool().await;
        let urls = vec![format!("{}/good.m3u", server.base)];
        let results = add_links(
            &pool,
            urls,
            true,
            &HashSet::new(),
            Arc::new(AtomicBool::new(true)),
            |_| panic!("a cancelled run must not report any result"),
        )
        .await;
        assert!(results.is_empty());
        assert!(db::list_playlists(&pool).await.unwrap().is_empty());
    }
}

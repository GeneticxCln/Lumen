//! Subtitles a site offers next to its video, as reported by yt-dlp. They are chosen by the
//! language the user prefers, a person's subtitles before machine captions, then fetched through
//! the same checks as any other subtitle.

use std::path::{Path, PathBuf};

use sqlx::{Pool, Sqlite};

use super::SubtitleCandidate;
use super::http::{self, Guard};
use super::lang;
use super::store::{self, MAX_SUBTITLE_BYTES};
use crate::error::Result;
use crate::extract::ExtractedSubtitle;

pub const PROVIDER: &str = "site";

/// The ISO 639-1 code of a site's language key (`en-US` is `en`).
fn code_of(key: &str) -> Option<&'static str> {
    lang::normalize(key).or_else(|| lang::normalize(key.split(['-', '_']).next().unwrap_or(key)))
}

/// The best track for the first of `languages` that has one: a person's subtitles, else captions
/// made by machine. `None` when no track is in a wanted language.
pub fn pick<'a>(
    tracks: &'a [ExtractedSubtitle],
    languages: &[String],
) -> Option<&'a ExtractedSubtitle> {
    languages.iter().find_map(|wanted| {
        let in_language =
            |track: &&ExtractedSubtitle| code_of(&track.language) == Some(wanted.as_str());
        tracks
            .iter()
            .filter(in_language)
            .find(|track| !track.automatic)
            .or_else(|| {
                tracks
                    .iter()
                    .filter(in_language)
                    .find(|track| track.automatic)
            })
    })
}

async fn fetch(
    client: &reqwest::Client,
    guard: &Guard,
    pool: &Pool<Sqlite>,
    app_dir: &Path,
    content_key: &str,
    track: &ExtractedSubtitle,
) -> Result<PathBuf> {
    let language = code_of(&track.language).unwrap_or("und");
    let url = guard.parse(&track.url)?;
    let bytes = http::get_bytes(client, guard, &url, MAX_SUBTITLE_BYTES).await?;
    let text = store::normalise(&bytes)?;
    let candidate = SubtitleCandidate {
        provider: PROVIDER.into(),
        id: format!("{language}{}", if track.automatic { "-auto" } else { "" }),
        language: language.into(),
        release: String::new(),
        hearing_impaired: false,
        downloads: None,
        fps: None,
        hash_match: false,
        score: 0,
        url: track.url.clone(),
    };
    let path = store::save(app_dir, content_key, &candidate, &text)?;
    super::service::remember_choice(pool, content_key, &candidate, &path).await?;
    Ok(path)
}

/// Downloads a track the site offered and stores it, remembering it for the title.
pub async fn fetch_and_store(
    pool: &Pool<Sqlite>,
    app_dir: &Path,
    content_key: &str,
    track: &ExtractedSubtitle,
    allow_lan: bool,
) -> Result<PathBuf> {
    let client = http::client(allow_lan)?;
    fetch(
        &client,
        &Guard::any_public_host(),
        pool,
        app_dir,
        content_key,
        track,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitles::testing::{Canned, serve};
    use sqlx::sqlite::SqlitePoolOptions;

    fn track(language: &str, automatic: bool) -> ExtractedSubtitle {
        ExtractedSubtitle {
            language: language.into(),
            ext: "vtt".into(),
            url: format!("https://cdn.example/{language}-{automatic}.vtt"),
            automatic,
        }
    }

    #[test]
    fn people_beat_machines_and_the_first_language_wins() {
        let tracks = [
            track("en", true),
            track("en-US", false),
            track("fr", false),
            track("de", true),
        ];
        let said = |codes: &[&str]| {
            codes
                .iter()
                .map(|code| code.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            pick(&tracks, &said(&["en"])).unwrap().language,
            "en-US",
            "manual before automatic"
        );
        assert_eq!(pick(&tracks, &said(&["fr", "en"])).unwrap().language, "fr");
        assert_eq!(
            pick(&tracks, &said(&["de"])).unwrap().language,
            "de",
            "captions when nothing better exists"
        );
        assert!(pick(&tracks, &said(&["ja"])).is_none());
        assert!(pick(&tracks, &[]).is_none());
        assert_eq!(code_of("pt-BR"), Some("pt"));
        assert_eq!(code_of("zh_Hans"), Some("zh"));
    }

    #[tokio::test]
    async fn a_site_track_is_fetched_checked_stored_and_remembered() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        let server = serve(vec![
            Canned::text(
                "/en.vtt",
                "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHello\n",
            ),
            Canned::text("/page.vtt", "<html>sign in to continue</html>"),
        ])
        .await;
        let dir = std::env::temp_dir().join(format!("lumen-site-subs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let client = http::client(true).unwrap();
        let guard = Guard::insecure_for_tests(&["127.0.0.1"]);

        let good = ExtractedSubtitle {
            language: "en".into(),
            ext: "vtt".into(),
            url: format!("{}/en.vtt", server.base),
            automatic: false,
        };
        let path = fetch(&client, &guard, &pool, &dir, "stream:abc", &good)
            .await
            .unwrap();
        assert_eq!(path.extension().unwrap(), "vtt");
        assert_eq!(
            crate::subtitles::service::chosen(&pool, "stream:abc", &["en".to_string()])
                .await
                .unwrap(),
            Some(path)
        );

        // A sign-in page where the subtitle should be is refused, not shown as dialogue.
        let page = ExtractedSubtitle {
            url: format!("{}/page.vtt", server.base),
            ..good.clone()
        };
        assert!(
            fetch(&client, &guard, &pool, &dir, "stream:def", &page)
                .await
                .is_err()
        );

        // The production guard only lets https through.
        let http_only = ExtractedSubtitle {
            url: format!("{}/en.vtt", server.base),
            ..good
        };
        let strict = Guard::any_public_host();
        assert!(
            fetch(&client, &strict, &pool, &dir, "stream:ghi", &http_only)
                .await
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

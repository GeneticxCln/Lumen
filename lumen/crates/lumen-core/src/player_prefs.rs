//! Per-profile audio and subtitle preferences, and how they map onto mpv.
//!
//! The mapping is a pure function so it can be tested without a player; the app applies the result
//! to mpv before each load and when the preferences change.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Pool, Sqlite};

use crate::db::{get_setting, set_setting};
use crate::error::{PlayerError, Result};

pub const MIN_SUB_SCALE: f64 = 0.5;
pub const MAX_SUB_SCALE: f64 = 3.0;
pub const MAX_SUB_DELAY_SECONDS: f64 = 600.0;
pub(crate) const MAX_SUBTITLE_FILE_BYTES: u64 = 10 * 1024 * 1024;
const SUBTITLE_EXTENSIONS: [&str; 5] = ["srt", "ass", "ssa", "vtt", "sub"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct PlayerPrefs {
    /// Preferred audio language as an ISO 639 code (`en`, `ja`, `por`); empty leaves the choice to mpv.
    pub audio_lang: String,
    /// `auto` (mpv's default choice), `off`, or a language code to show when a track has it.
    pub subtitles: String,
    /// Subtitle size, 1.0 being mpv's default.
    pub sub_scale: f64,
    /// Vertical position: 100 is the bottom of the picture, 0 the top.
    pub sub_pos: i64,
}

impl Default for PlayerPrefs {
    fn default() -> Self {
        Self {
            audio_lang: String::new(),
            subtitles: "auto".into(),
            sub_scale: 1.0,
            sub_pos: 100,
        }
    }
}

fn clean_language(raw: &str) -> Result<String> {
    let code = raw.trim().to_ascii_lowercase();
    let valid = (2..=3).contains(&code.len()) && code.chars().all(|c| c.is_ascii_lowercase());
    if valid {
        Ok(code)
    } else {
        Err(PlayerError::InvalidArg(format!(
            "\"{raw}\" is not a language code (use e.g. en, ja, por)"
        )))
    }
}

impl PlayerPrefs {
    /// Checks and normalises preferences coming from the UI.
    pub fn validated(&self) -> Result<Self> {
        let audio_lang = if self.audio_lang.trim().is_empty() {
            String::new()
        } else {
            clean_language(&self.audio_lang)?
        };
        let subtitles = match self.subtitles.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => "auto".to_string(),
            "off" => "off".to_string(),
            other => clean_language(other)?,
        };
        if !self.sub_scale.is_finite() || !(MIN_SUB_SCALE..=MAX_SUB_SCALE).contains(&self.sub_scale)
        {
            return Err(PlayerError::InvalidArg(format!(
                "Subtitle size must be between {MIN_SUB_SCALE} and {MAX_SUB_SCALE}"
            )));
        }
        if !(0..=100).contains(&self.sub_pos) {
            return Err(PlayerError::InvalidArg(
                "Subtitle position must be between 0 and 100".into(),
            ));
        }
        // Rounded so a slider's float noise does not leak into the saved value.
        Ok(Self {
            audio_lang,
            subtitles,
            sub_scale: (self.sub_scale * 100.0).round() / 100.0,
            sub_pos: self.sub_pos,
        })
    }

    /// The mpv properties that express these preferences. `reset_delay` also zeroes the subtitle
    /// delay, which belongs to one file and must not carry over to the next.
    pub fn mpv_properties(&self, reset_delay: bool) -> Vec<(&'static str, Value)> {
        let list = |code: &str| {
            if code.is_empty() {
                json!([])
            } else {
                json!([code])
            }
        };
        let mut properties = vec![
            ("alang", list(&self.audio_lang)),
            (
                "slang",
                list(if matches!(self.subtitles.as_str(), "auto" | "off") {
                    ""
                } else {
                    &self.subtitles
                }),
            ),
            ("sub-visibility", json!(self.subtitles != "off")),
            ("sub-scale", json!(self.sub_scale)),
            ("sub-pos", json!(self.sub_pos)),
        ];
        if reset_delay {
            properties.push(("sub-delay", json!(0.0)));
        }
        properties
    }
}

fn key(profile_id: i64) -> String {
    format!("player_prefs_{profile_id}")
}

pub async fn load(pool: &Pool<Sqlite>, profile_id: i64) -> Result<PlayerPrefs> {
    let Some(raw) = get_setting(pool, &key(profile_id)).await? else {
        return Ok(PlayerPrefs::default());
    };
    // A value that no longer validates (hand-edited, or from a future version) falls back to the
    // defaults rather than breaking playback.
    Ok(serde_json::from_str::<PlayerPrefs>(&raw)
        .ok()
        .and_then(|prefs| prefs.validated().ok())
        .unwrap_or_default())
}

pub async fn save(
    pool: &Pool<Sqlite>,
    profile_id: i64,
    prefs: &PlayerPrefs,
) -> Result<PlayerPrefs> {
    let clean = prefs.validated()?;
    set_setting(pool, &key(profile_id), &serde_json::to_string(&clean)?).await?;
    Ok(clean)
}

/// A subtitle offset in seconds, limited to what is sensible (±10 minutes).
pub fn clamp_delay(seconds: f64) -> Result<f64> {
    if !seconds.is_finite() {
        return Err(PlayerError::InvalidArg(
            "Subtitle delay must be a number".into(),
        ));
    }
    Ok((seconds.clamp(-MAX_SUB_DELAY_SECONDS, MAX_SUB_DELAY_SECONDS) * 10.0).round() / 10.0)
}

/// Checks a subtitle file the user picked: a real local file with a subtitle extension and a
/// sensible size. Returns its canonical path, which is what mpv is given.
pub fn validate_subtitle_file(path: &str) -> Result<PathBuf> {
    let raw = Path::new(path.trim());
    let extension = raw
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !SUBTITLE_EXTENSIONS.contains(&extension.as_str()) {
        return Err(PlayerError::InvalidArg(
            "Choose a subtitle file (.srt, .ass, .ssa, .vtt or .sub)".into(),
        ));
    }
    let canonical = raw
        .canonicalize()
        .map_err(|_| PlayerError::NotFound("That subtitle file could not be found".into()))?;
    let metadata = std::fs::metadata(&canonical)?;
    if !metadata.is_file() {
        return Err(PlayerError::InvalidArg("That is not a file".into()));
    }
    if metadata.len() > MAX_SUBTITLE_FILE_BYTES {
        return Err(PlayerError::InvalidArg(
            "That subtitle file is too large".into(),
        ));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    #[test]
    fn preferences_are_validated_and_normalised() {
        let prefs = PlayerPrefs {
            audio_lang: " JA ".into(),
            subtitles: "EN".into(),
            sub_scale: 1.2345,
            sub_pos: 90,
        }
        .validated()
        .unwrap();
        assert_eq!(
            (
                prefs.audio_lang.as_str(),
                prefs.subtitles.as_str(),
                prefs.sub_scale,
                prefs.sub_pos
            ),
            ("ja", "en", 1.23, 90)
        );
        assert_eq!(
            PlayerPrefs {
                subtitles: "".into(),
                ..Default::default()
            }
            .validated()
            .unwrap()
            .subtitles,
            "auto"
        );
        for bad in [
            PlayerPrefs {
                audio_lang: "english".into(),
                ..Default::default()
            },
            PlayerPrefs {
                audio_lang: "e1".into(),
                ..Default::default()
            },
            PlayerPrefs {
                subtitles: "x".into(),
                ..Default::default()
            },
            PlayerPrefs {
                sub_scale: 0.1,
                ..Default::default()
            },
            PlayerPrefs {
                sub_scale: f64::NAN,
                ..Default::default()
            },
            PlayerPrefs {
                sub_scale: 9.0,
                ..Default::default()
            },
            PlayerPrefs {
                sub_pos: 101,
                ..Default::default()
            },
            PlayerPrefs {
                sub_pos: -1,
                ..Default::default()
            },
        ] {
            assert!(bad.validated().is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn preferences_map_to_mpv_properties() {
        let get = |props: &[(&str, Value)], name: &str| {
            props
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.clone())
        };
        let defaults = PlayerPrefs::default().mpv_properties(false);
        assert_eq!(
            get(&defaults, "alang"),
            Some(json!([])),
            "no preference clears any earlier choice"
        );
        assert_eq!(get(&defaults, "slang"), Some(json!([])));
        assert_eq!(get(&defaults, "sub-visibility"), Some(json!(true)));
        assert_eq!(
            get(&defaults, "sub-delay"),
            None,
            "delay is only reset on load"
        );

        let japanese = PlayerPrefs {
            audio_lang: "ja".into(),
            subtitles: "en".into(),
            sub_scale: 1.5,
            sub_pos: 95,
        }
        .mpv_properties(true);
        assert_eq!(get(&japanese, "alang"), Some(json!(["ja"])));
        assert_eq!(get(&japanese, "slang"), Some(json!(["en"])));
        assert_eq!(get(&japanese, "sub-scale"), Some(json!(1.5)));
        assert_eq!(get(&japanese, "sub-pos"), Some(json!(95)));
        assert_eq!(get(&japanese, "sub-delay"), Some(json!(0.0)));

        let off = PlayerPrefs {
            subtitles: "off".into(),
            ..Default::default()
        }
        .mpv_properties(false);
        assert_eq!(get(&off, "sub-visibility"), Some(json!(false)));
        assert_eq!(get(&off, "slang"), Some(json!([])));
    }

    #[tokio::test]
    async fn preferences_are_stored_per_profile_and_bad_stored_values_fall_back() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        assert_eq!(load(&pool, 1).await.unwrap(), PlayerPrefs::default());
        save(
            &pool,
            1,
            &PlayerPrefs {
                audio_lang: "fr".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(load(&pool, 1).await.unwrap().audio_lang, "fr");
        assert_eq!(
            load(&pool, 2).await.unwrap(),
            PlayerPrefs::default(),
            "another profile is unaffected"
        );
        assert!(
            save(
                &pool,
                1,
                &PlayerPrefs {
                    sub_scale: 50.0,
                    ..Default::default()
                }
            )
            .await
            .is_err()
        );
        assert_eq!(
            load(&pool, 1).await.unwrap().audio_lang,
            "fr",
            "a refused save changes nothing"
        );
        set_setting(&pool, "player_prefs_3", "{\"subScale\": 99}")
            .await
            .unwrap();
        assert_eq!(load(&pool, 3).await.unwrap(), PlayerPrefs::default());
        set_setting(&pool, "player_prefs_4", "not json")
            .await
            .unwrap();
        assert_eq!(load(&pool, 4).await.unwrap(), PlayerPrefs::default());
    }

    #[test]
    fn delays_are_rounded_and_limited() {
        assert_eq!(clamp_delay(0.25).unwrap(), 0.3);
        assert_eq!(clamp_delay(-1.04).unwrap(), -1.0);
        assert_eq!(clamp_delay(10_000.0).unwrap(), 600.0);
        assert!(clamp_delay(f64::INFINITY).is_err());
    }

    #[test]
    fn only_real_subtitle_files_are_accepted() {
        let dir =
            std::env::temp_dir().join(format!("lumen-subs-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let srt = dir.join("movie.SRT");
        std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:02,000\nHi\n").unwrap();
        assert!(validate_subtitle_file(srt.to_str().unwrap()).is_ok());
        let txt = dir.join("notes.txt");
        std::fs::write(&txt, "x").unwrap();
        assert!(
            validate_subtitle_file(txt.to_str().unwrap()).is_err(),
            "wrong extension"
        );
        assert!(validate_subtitle_file(dir.join("missing.srt").to_str().unwrap()).is_err());
        assert!(
            validate_subtitle_file("http://evil.example/a.srt").is_err(),
            "URLs are not files"
        );
        let folder = dir.join("folder.srt");
        std::fs::create_dir_all(&folder).unwrap();
        assert!(
            validate_subtitle_file(folder.to_str().unwrap()).is_err(),
            "a directory is not a file"
        );
        let big = dir.join("big.srt");
        std::fs::write(&big, vec![b'a'; (MAX_SUBTITLE_FILE_BYTES + 1) as usize]).unwrap();
        assert!(validate_subtitle_file(big.to_str().unwrap()).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}

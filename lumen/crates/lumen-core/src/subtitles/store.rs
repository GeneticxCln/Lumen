//! What happens to a downloaded subtitle before the player sees it: it is checked to really be
//! one, converted to UTF-8, saved in the app's cache under a name made only of safe characters,
//! and old files are dropped when the cache grows past its cap.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::SubtitleCandidate;
use crate::error::{PlayerError, Result};
use crate::player_prefs::MAX_SUBTITLE_FILE_BYTES;

/// Largest subtitle accepted, the same limit the player applies to files the user picks.
pub const MAX_SUBTITLE_BYTES: usize = MAX_SUBTITLE_FILE_BYTES as usize;
/// The cache of downloaded subtitles is trimmed to this size.
pub const CACHE_LIMIT_BYTES: u64 = 50 * 1024 * 1024;

fn not_a_subtitle() -> PlayerError {
    PlayerError::Parse("What the service sent is not a subtitle file".into())
}

/// The text of a downloaded file as UTF-8. Handles a byte-order mark, UTF-16 and, failing that,
/// detects the old single-byte or CJK encoding. Refuses anything that is not subtitle text, such
/// as an HTML error page or a zip.
pub fn normalise(bytes: &[u8]) -> Result<String> {
    if bytes.is_empty() || bytes.len() > MAX_SUBTITLE_BYTES {
        return Err(not_a_subtitle());
    }
    let decoded: String = if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8(rest.to_vec()).map_err(|_| not_a_subtitle())?
    } else if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let (encoding, _) = encoding_rs::Encoding::for_bom(bytes).ok_or_else(not_a_subtitle)?;
        let (text, _, had_errors) = encoding.decode(bytes);
        if had_errors {
            return Err(not_a_subtitle());
        }
        text.into_owned()
    } else if let Ok(text) = std::str::from_utf8(bytes) {
        text.to_string()
    } else {
        let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
        detector.feed(bytes, true);
        let encoding = detector.guess(None, chardetng::Utf8Detection::Deny);
        let (text, _, had_errors) = encoding.decode(bytes);
        if had_errors {
            return Err(not_a_subtitle());
        }
        text.into_owned()
    };
    if looks_like_subtitle(&decoded) {
        Ok(decoded)
    } else {
        Err(not_a_subtitle())
    }
}

fn looks_like_subtitle(text: &str) -> bool {
    let head: String = text.chars().take(64 * 1024).collect();
    let lower = head.to_ascii_lowercase();
    if lower.contains("<html") || lower.contains("<!doctype") {
        return false;
    }
    head.contains("-->")
        || head.contains("[Script Info]")
        || head.contains("Dialogue:")
        || head.starts_with("WEBVTT")
}

/// The extension that makes the player read it as the right format.
pub fn extension_for(text: &str) -> &'static str {
    let head: String = text.chars().take(4096).collect();
    if head.contains("[Script Info]") || head.contains("[V4+ Styles]") {
        "ass"
    } else if head.trim_start().starts_with("WEBVTT") {
        "vtt"
    } else {
        "srt"
    }
}

/// A short stable folder name for a content key (keys contain `:` and arbitrary text).
fn folder_for(content_key: &str) -> String {
    let digest = Sha256::digest(content_key.as_bytes());
    digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn safe_part(text: &str) -> String {
    let kept: String = text
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(40)
        .collect();
    if kept.is_empty() || kept.len() != text.chars().count() {
        let digest = Sha256::digest(text.as_bytes());
        digest
            .iter()
            .take(6)
            .map(|byte| format!("{byte:02x}"))
            .collect()
    } else {
        kept
    }
}

/// Writes `text` to `<base>/subtitles/<key folder>/<language>-<provider>-<id>.<ext>` and returns
/// the path. The name is built only from characters that cannot form a path.
pub fn save(
    base: &Path,
    content_key: &str,
    candidate: &SubtitleCandidate,
    text: &str,
) -> Result<PathBuf> {
    let directory = base.join("subtitles").join(folder_for(content_key));
    fs::create_dir_all(&directory)?;
    let name = format!(
        "{}-{}-{}.{}",
        safe_part(&candidate.language),
        safe_part(&candidate.provider),
        safe_part(&candidate.id),
        extension_for(text)
    );
    let path = directory.join(&name);
    let partial = directory.join(format!("{name}.part"));
    fs::write(&partial, text)?;
    fs::rename(&partial, &path)?;
    prune(&base.join("subtitles"), CACHE_LIMIT_BYTES);
    Ok(path)
}

/// Deletes the oldest files until the folder is at most `limit` bytes. Best effort.
pub fn prune(root: &Path, limit: u64) {
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    let Ok(folders) = fs::read_dir(root) else {
        return;
    };
    for folder in folders.flatten() {
        let Ok(entries) = fs::read_dir(folder.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            if let Ok(metadata) = entry.metadata()
                && metadata.is_file()
            {
                files.push((
                    metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                    metadata.len(),
                    entry.path(),
                ));
            }
        }
    }
    let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
    files.sort_by_key(|(modified, _, _)| *modified);
    for (_, size, path) in files {
        if total <= limit {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total -= size;
            if let Some(parent) = path.parent() {
                let _ = fs::remove_dir(parent);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRT: &str = "1\n00:00:01,000 --> 00:00:02,000\nCafé crème\n";

    fn candidate(id: &str) -> SubtitleCandidate {
        SubtitleCandidate {
            provider: "gestdown".into(),
            id: id.into(),
            language: "en".into(),
            release: String::new(),
            hearing_impaired: false,
            downloads: None,
            fps: None,
            hash_match: false,
            score: 0,
            url: String::new(),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lumen-subs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn utf8_with_and_without_a_bom_passes_through() {
        assert_eq!(normalise(SRT.as_bytes()).unwrap(), SRT);
        let mut with_bom = vec![0xEF, 0xBB, 0xBF];
        with_bom.extend_from_slice(SRT.as_bytes());
        assert_eq!(normalise(&with_bom).unwrap(), SRT);
    }

    #[test]
    fn utf16_and_legacy_encodings_become_utf8() {
        let mut utf16le = vec![0xFF, 0xFE];
        utf16le.extend(SRT.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(normalise(&utf16le).unwrap(), SRT);
        let mut utf16be = vec![0xFE, 0xFF];
        utf16be.extend(SRT.encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(normalise(&utf16be).unwrap(), SRT);

        // Windows-1252, the encoding most old English-language files use: "Café crème".
        let latin: Vec<u8> = b"1\n00:00:01,000 --> 00:00:02,000\nCaf\xe9 cr\xe8me and more text so the guess is steady\n".to_vec();
        let text = normalise(&latin).unwrap();
        assert!(text.contains("Café crème"), "{text}");
    }

    #[test]
    fn things_that_are_not_subtitles_are_refused() {
        for bad in [
            &b""[..],
            b"<!DOCTYPE html><html><body>Error 404 --> nope</body></html>",
            b"PK\x03\x04 zip bytes",
            b"just some words with no timing at all",
        ] {
            assert!(
                normalise(bad).is_err(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
        assert!(normalise(&vec![b'a'; MAX_SUBTITLE_BYTES + 1]).is_err());
    }

    #[test]
    fn the_format_is_read_from_the_text() {
        assert_eq!(extension_for(SRT), "srt");
        assert_eq!(
            extension_for("WEBVTT\n\n00:01.000 --> 00:02.000\nhi"),
            "vtt"
        );
        assert_eq!(
            extension_for("[Script Info]\nTitle: x\n[Events]\nDialogue: 0,0:00:01.00"),
            "ass"
        );
    }

    #[test]
    fn files_are_saved_under_safe_names() {
        let base = scratch("save");
        let path = save(&base, "tmdb:movie:603", &candidate("57dd12a4"), SRT).unwrap();
        assert!(path.starts_with(base.join("subtitles")));
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            "en-gestdown-57dd12a4.srt"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), SRT);

        // An id with path characters never reaches the file system as written.
        let hostile = save(&base, "x", &candidate("../../../etc/passwd"), SRT).unwrap();
        assert!(hostile.starts_with(base.join("subtitles")), "{hostile:?}");
        assert!(!hostile.to_str().unwrap().contains(".."));
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn the_cache_drops_its_oldest_files_first() {
        let base = scratch("prune");
        let root = base.join("subtitles");
        let folder = root.join("abc");
        fs::create_dir_all(&folder).unwrap();
        for (index, name) in ["old", "middle", "new"].iter().enumerate() {
            let path = folder.join(name);
            fs::write(&path, vec![b'x'; 100]).unwrap();
            let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + index as u64);
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(when)
                .unwrap();
        }
        prune(&root, 250);
        assert!(!folder.join("old").exists());
        assert!(folder.join("middle").exists() && folder.join("new").exists());
        prune(&root, 0);
        assert!(!root.join("abc").exists(), "empty folders go too");
        fs::remove_dir_all(&base).unwrap();
    }
}

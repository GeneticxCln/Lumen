//! Orders subtitle candidates so the best one is first: the language the user asked for, then
//! an exact file match, then how closely the release matches the file being played.

use std::collections::HashSet;

use super::{SubtitleCandidate, SubtitleQuery};

/// Words of a release name that say what it is, not what it is called.
fn tokens(name: &str) -> HashSet<String> {
    name.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| token.len() > 1)
        .map(str::to_string)
        .collect()
}

/// The release group, the text after the last dash of the name without its extension.
fn group(name: &str) -> Option<String> {
    let stem = name.rsplit_once('.').map_or(
        name,
        |(rest, extension)| {
            if extension.len() <= 4 { rest } else { name }
        },
    );
    let (_, group) = stem.rsplit_once('-')?;
    let group = group.trim().to_ascii_lowercase();
    (!group.is_empty() && group.len() <= 16 && !group.contains(' ')).then_some(group)
}

/// How alike two release names are, 0 to 1 (shared words over all words).
pub fn similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (tokens(a), tokens(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let shared = a.intersection(&b).count() as f64;
    shared / a.union(&b).count() as f64
}

pub fn score(query: &SubtitleQuery, candidate: &SubtitleCandidate) -> i32 {
    let mut score = 0;
    match query
        .languages
        .iter()
        .position(|code| *code == candidate.language)
    {
        // Tiers wide enough that no release match (at most about 1400) can lift a worse language
        // above a better one.
        Some(index) => score += 3000 - 1000 * index.min(2) as i32,
        None => score -= 3000,
    }
    if candidate.hash_match {
        score += 800;
    }
    if let Some(played) = &query.release_name {
        score += (similarity(played, &candidate.release) * 400.0) as i32;
        if let (Some(a), Some(b)) = (group(played), group(&candidate.release))
            && a == b
        {
            score += 150;
        }
    }
    if candidate.hearing_impaired {
        score -= 50;
    }
    if let Some(downloads) = candidate.downloads {
        score += ((downloads as f64 + 1.0).log10().min(5.0) * 10.0) as i32;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(language: &str, release: &str) -> SubtitleCandidate {
        SubtitleCandidate {
            provider: "test".into(),
            id: release.into(),
            language: language.into(),
            release: release.into(),
            hearing_impaired: false,
            downloads: None,
            fps: None,
            hash_match: false,
            score: 0,
            url: String::new(),
        }
    }

    fn query(languages: &[&str], release: Option<&str>) -> SubtitleQuery {
        SubtitleQuery {
            languages: languages.iter().map(|code| code.to_string()).collect(),
            release_name: release.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn the_requested_language_beats_everything_else() {
        let q = query(&["fr", "en"], Some("Heat.1995.1080p.BluRay.x264-GRP.mkv"));
        let exact_english = candidate("en", "Heat.1995.1080p.BluRay.x264-GRP");
        let vague_french = candidate("fr", "Heat");
        let german = candidate("de", "Heat.1995.1080p.BluRay.x264-GRP");
        assert!(
            score(&q, &vague_french) > score(&q, &exact_english),
            "first choice first"
        );
        assert!(
            score(&q, &exact_english) > score(&q, &german),
            "unwanted languages last"
        );
    }

    #[test]
    fn closer_releases_and_exact_file_matches_rank_higher() {
        let q = query(
            &["en"],
            Some("Severance.S01E01.2160p.ATVP.WEB-DL.DDP5.1-TEPES.mkv"),
        );
        let same = candidate(
            "en",
            "Severance.S01E01.2160p.ATVP.WEB-DL.DDP5.1.Atmos-TEPES",
        );
        let other = candidate("en", "Severance.S01E01.720p.HDTV.x264-KILLERS");
        assert!(score(&q, &same) > score(&q, &other));

        let mut hashed = candidate("en", "something else entirely");
        hashed.hash_match = true;
        assert!(
            score(&q, &hashed) > score(&q, &same),
            "an exact file match wins"
        );
    }

    #[test]
    fn hearing_impaired_is_a_small_penalty_and_downloads_a_small_bonus() {
        let q = query(&["en"], None);
        let plain = candidate("en", "a");
        let mut impaired = candidate("en", "a");
        impaired.hearing_impaired = true;
        assert!(score(&q, &plain) > score(&q, &impaired));
        let mut popular = candidate("en", "a");
        popular.downloads = Some(100_000);
        assert!(score(&q, &popular) > score(&q, &plain));
    }

    #[test]
    fn similarity_is_symmetric_and_bounded() {
        let a = "Heat.1995.1080p.BluRay-GRP";
        let b = "Heat 1995 720p BluRay-OTHER";
        assert_eq!(similarity(a, b), similarity(b, a));
        assert!((0.0..=1.0).contains(&similarity(a, b)));
        assert_eq!(similarity(a, a), 1.0);
        assert_eq!(similarity("", a), 0.0);
        assert_eq!(
            group("Heat.1995.BluRay.x264-GRP.mkv").as_deref(),
            Some("grp")
        );
        assert_eq!(group("no group here"), None);
    }
}

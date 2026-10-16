//! Turns a file or release name into the pieces a subtitle lookup needs: the title, the year and
//! the season and episode.
//!
//! `Heat.1995.1080p.BluRay.x264-GRP.mkv` and `Severance S01E02 Half Loop` both appear in
//! libraries; the title is everything before the first year, episode marker or quality tag.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::content_key::parse_season_episode;

static YEAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(19\d{2}|20\d{2})\b").expect("valid year regex"));
static EPISODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bS\d{1,2}[ ._\-]*E\d{1,3}\b|\b\d{1,2}x\d{2,3}\b")
        .expect("valid episode regex")
});
static ALT_EPISODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(\d{1,2})x(\d{2,3})\b").expect("valid alt episode regex"));
static QUALITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(480p|576p|720p|1080p|1440p|2160p|4k|uhd|bluray|blu-ray|brrip|bdrip|web-?dl|webrip|hdtv|dvdrip|x264|x265|h264|h265|hevc|remux|proper|repack|extended|unrated|imax)\b",
    )
    .expect("valid quality regex")
});
static BRACKETED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[[^\]]*\]").expect("valid bracket regex"));
static PAREN_YEAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\((19\d{2}|20\d{2})\)").expect("valid paren year regex"));

const EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "mov", "m4v", "webm", "ts", "m2ts", "wmv", "flv", "mpg", "mpeg", "srt",
];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Parsed {
    pub title: String,
    pub year: Option<i32>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
}

/// The file name without folders and without a known video or subtitle extension.
fn stem(name: &str) -> String {
    let file = Path::new(name)
        .file_name()
        .and_then(|part| part.to_str())
        .unwrap_or(name);
    if let Some((rest, extension)) = file.rsplit_once('.')
        && EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
    {
        return rest.to_string();
    }
    file.to_string()
}

pub fn parse_release(name: &str) -> Parsed {
    let stem = stem(name);
    let spaced: String = stem
        .chars()
        .map(|c| if c == '.' || c == '_' { ' ' } else { c })
        .collect();
    let paren_year = PAREN_YEAR
        .captures(&spaced)
        .and_then(|caps| caps[1].parse::<i32>().ok());
    let text = BRACKETED.replace_all(&spaced, " ");
    let text = PAREN_YEAR.replace_all(&text, " ");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");

    let (season, episode) = parse_season_episode(&text)
        .or_else(|| {
            ALT_EPISODE
                .captures(&text)
                .and_then(|caps| Some((caps[1].parse().ok()?, caps[2].parse().ok()?)))
        })
        .map(|(season, episode)| (Some(season), Some(episode)))
        .unwrap_or((None, None));

    // A year at the very start is part of the title ("2012", "1917").
    let year_match = YEAR.find_iter(&text).find(|found| found.start() > 0);
    let mut end = text.len();
    for stop in [
        year_match.map(|found| found.start()),
        EPISODE.find(&text).map(|found| found.start()),
        QUALITY.find(&text).map(|found| found.start()),
    ]
    .into_iter()
    .flatten()
    {
        end = end.min(stop);
    }
    let title = text[..end]
        .trim_matches(|c: char| c == '-' || c == ':' || c == '|' || c.is_whitespace())
        .to_string();
    let year = paren_year.or_else(|| year_match.and_then(|found| found.as_str().parse().ok()));

    Parsed {
        title,
        year,
        season,
        episode,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(name: &str) -> (String, Option<i32>, Option<u32>, Option<u32>) {
        let result = parse_release(name);
        (result.title, result.year, result.season, result.episode)
    }

    #[test]
    fn dotted_release_names() {
        assert_eq!(
            parsed("Heat.1995.1080p.BluRay.x264-GRP.mkv"),
            ("Heat".into(), Some(1995), None, None)
        );
        assert_eq!(
            parsed("/media/The.Shawshank.Redemption.1994.720p.BluRay.x264-SiNNERS.mkv"),
            ("The Shawshank Redemption".into(), Some(1994), None, None)
        );
    }

    #[test]
    fn episodes_in_either_notation() {
        assert_eq!(
            parsed("Severance S01E02 Half Loop"),
            ("Severance".into(), None, Some(1), Some(2))
        );
        assert_eq!(
            parsed("Severance.S02E10.Cold.Harbor.2160p.ATVP.WEB-DL-TEPES.mkv"),
            ("Severance".into(), None, Some(2), Some(10))
        );
        assert_eq!(
            parsed("Fargo 1x03 The Rooster Prophecy"),
            ("Fargo".into(), None, Some(1), Some(3))
        );
        assert_eq!(
            parsed("Doctor Who 2005 S01E01"),
            ("Doctor Who".into(), Some(2005), Some(1), Some(1))
        );
    }

    #[test]
    fn years_and_brackets() {
        assert_eq!(
            parsed("Heat (1995)"),
            ("Heat".into(), Some(1995), None, None)
        );
        assert_eq!(
            parsed("[YTS] Arrival [1080p] (2016)"),
            ("Arrival".into(), Some(2016), None, None)
        );
        // A year that is the whole title is not a year.
        assert_eq!(
            parsed("2012.1080p.BluRay.mkv"),
            ("2012".into(), None, None, None)
        );
        assert_eq!(
            parsed("1917 2019 1080p"),
            ("1917".into(), Some(2019), None, None)
        );
    }

    #[test]
    fn plain_names_stay_whole() {
        assert_eq!(
            parsed("Big Buck Bunny"),
            ("Big Buck Bunny".into(), None, None, None)
        );
        assert_eq!(parsed(""), (String::new(), None, None, None));
    }
}

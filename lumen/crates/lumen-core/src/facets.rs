//! Derived browse facets: content kind, adult flag, year, group tags and genre tags.
//!
//! The frontend has always derived these from whatever channels it had loaded. That cannot work
//! for a 14.76M-row library, so the same rules live here and the UI reads completed counts from
//! the database. Tests in this module mirror the frontend fixtures so the two derivations agree on
//! the cases users actually see: packed `;` group tags, TMDB genres, manual overrides and the
//! keyword fallbacks.

use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FacetValues {
    pub kind: &'static str,
    pub adult: bool,
    pub year: String,
    pub groups: Vec<String>,
    pub genres: Vec<String>,
}

struct GenreRule {
    pattern: Regex,
    genre: &'static str,
}

/// Order is significant: the frontend picks the first matching rule for each group tag.
static GENRE_RULES: LazyLock<Vec<GenreRule>> = LazyLock::new(|| {
    [
        (r"news", "News"),
        (r"sport", "Sports"),
        (r"documentary", "Documentary"),
        (r"music", "Music"),
        (r"movie|film|cinema", "Movie"),
        (r"series|shows?|soap|sitcom", "Series"),
        (r"kids?|child|family|cartoon|animat", "Family"),
        (r"science|education|space|learn", "Science"),
        (r"religio|faith|church|gospel", "Religious"),
        (r"shop|teleshop", "Shopping"),
        (r"travel|outdoor|nature", "Travel"),
        (r"legislat|politic|government|parliament", "Politics"),
        (r"comedy", "Comedy"),
        (r"lifestyle|cooking|food|garden", "Lifestyle"),
        (r"weather", "Weather"),
        (r"business|finance|money|stock", "Business"),
        (r"entertainment|general", "Entertainment"),
        (r"culture|art", "Culture"),
        (r"health|medical", "Health"),
        (r"auto|motor|car", "Motors"),
        (r"demo", "Demo"),
    ]
    .into_iter()
    .map(|(pattern, genre)| GenreRule {
        pattern: Regex::new(&format!("(?i){pattern}")).expect("static genre regex"),
        genre,
    })
    .collect()
});

static SERIES_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(series|show|shows|season|episode|episodes|sitcom|soap|anime)\b")
        .expect("static series regex")
});
static MOVIE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(movie|movies|film|films|cinema|feature|vod|demo|documentary|short)\b")
        .expect("static movie regex")
});
static ADULT_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(xxx|porn(?:ographic)?|adult|18\+|nsfw|erotic(?:a)?)\b")
        .expect("static adult regex")
});

fn tags(value: Option<&str>) -> Vec<String> {
    let mut values = Vec::new();
    for token in value.unwrap_or("").split(';') {
        let token = token.trim();
        if !token.is_empty() && !values.iter().any(|seen: &String| seen == token) {
            values.push(token.to_string());
        }
    }
    values
}

pub fn groups_of(group: Option<&str>) -> Vec<String> {
    let groups = tags(group);
    if groups.is_empty() {
        vec!["Other".to_string()]
    } else {
        groups
    }
}

pub fn infer_genres(group: Option<&str>, name: &str) -> Vec<String> {
    let mut genres = Vec::new();
    for token in groups_of(group) {
        let inferred = GENRE_RULES
            .iter()
            .find(|rule| rule.pattern.is_match(&token))
            .map(|rule| rule.genre.to_string())
            .or_else(|| (token != "Other").then_some(token));
        if let Some(genre) = inferred
            && !genres.contains(&genre)
        {
            genres.push(genre);
        }
    }
    if !genres.is_empty() {
        return genres;
    }
    for rule in GENRE_RULES.iter() {
        if rule.pattern.is_match(name) && !genres.iter().any(|seen| seen == rule.genre) {
            genres.push(rule.genre.to_string());
        }
    }
    if genres.is_empty() {
        genres.push("Uncategorized".to_string());
    }
    genres
}

pub fn genres_of(
    genre_names: Option<&str>,
    override_genres: Option<&str>,
    group: Option<&str>,
    name: &str,
) -> Vec<String> {
    let from_tmdb = tags(genre_names);
    if !from_tmdb.is_empty() {
        return from_tmdb;
    }
    let from_override = tags(override_genres);
    if !from_override.is_empty() {
        return from_override;
    }
    infer_genres(group, name)
}

pub fn content_kind(
    tmdb_type: Option<&str>,
    override_kind: Option<&str>,
    group: Option<&str>,
    name: &str,
    is_live: Option<bool>,
) -> &'static str {
    if tmdb_type == Some("movie") {
        return "movie";
    }
    if tmdb_type == Some("tv") {
        return "series";
    }
    if let Some(kind) = override_kind
        && (kind == "movie" || kind == "series" || kind == "live")
    {
        return match kind {
            "movie" => "movie",
            "series" => "series",
            _ => "live",
        };
    }
    let text = format!("{} {}", group.unwrap_or(""), name);
    if SERIES_PATTERN.is_match(&text) {
        return "series";
    }
    if MOVIE_PATTERN.is_match(&text) {
        return "movie";
    }
    if is_live == Some(false) {
        return "movie";
    }
    "live"
}

pub fn is_adult(group: Option<&str>, name: &str) -> bool {
    ADULT_PATTERN.is_match(&format!("{} {}", group.unwrap_or(""), name))
}

#[allow(clippy::too_many_arguments)]
pub fn values(
    tmdb_type: Option<&str>,
    override_kind: Option<&str>,
    group: Option<&str>,
    name: &str,
    is_live: Option<bool>,
    genre_names: Option<&str>,
    override_genres: Option<&str>,
    release_year: Option<i64>,
) -> FacetValues {
    FacetValues {
        kind: content_kind(tmdb_type, override_kind, group, name, is_live),
        adult: is_adult(group, name),
        year: release_year
            .filter(|year| *year > 0)
            .map(|year| year.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        groups: groups_of(group),
        genres: genres_of(genre_names, override_genres, group, name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_groups_and_genre_fallbacks_match_the_frontend() {
        let values = values(
            None,
            None,
            Some("Culture;Entertainment;Music;News"),
            "Evening News",
            Some(true),
            None,
            None,
            None,
        );
        assert_eq!(values.groups, ["Culture", "Entertainment", "Music", "News"]);
        assert_eq!(values.genres, ["Culture", "Entertainment", "Music", "News"]);
        assert_eq!(values.year, "unknown");
        assert_eq!(values.kind, "live");
    }

    #[test]
    fn tmdb_and_override_metadata_win_over_inference() {
        let tmdb = values(
            Some("movie"),
            Some("series"),
            Some("News"),
            "Heat",
            Some(false),
            Some("Drama;Crime;Drama"),
            Some("Comedy"),
            Some(1995),
        );
        assert_eq!(tmdb.kind, "movie");
        assert_eq!(tmdb.genres, ["Drama", "Crime"]);
        assert_eq!(tmdb.year, "1995");

        let overridden = values(
            None,
            Some("series"),
            Some("News"),
            "Severance",
            Some(true),
            None,
            Some("Drama;Mystery"),
            None,
        );
        assert_eq!(overridden.kind, "series");
        assert_eq!(overridden.genres, ["Drama", "Mystery"]);
    }

    #[test]
    fn unknown_groups_and_names_get_reachable_facets() {
        let values = values(
            None,
            None,
            None,
            "Cooking Show",
            Some(true),
            None,
            None,
            Some(0),
        );
        assert_eq!(values.groups, ["Other"]);
        assert_eq!(values.genres, ["Series", "Lifestyle"]);
        assert_eq!(values.year, "unknown");
        assert!(!values.adult);
    }

    #[test]
    fn adult_and_kind_keywords_follow_the_frontend_regexes() {
        let adult = values(
            None,
            None,
            Some("Adult"),
            "Late Night",
            Some(true),
            None,
            None,
            None,
        );
        assert!(adult.adult);
        assert_eq!(adult.kind, "live");

        let fixed = values(
            None,
            None,
            Some("Films"),
            "A Short Film",
            Some(false),
            None,
            None,
            None,
        );
        assert_eq!(fixed.kind, "movie");
        assert!(!fixed.adult);
    }
}

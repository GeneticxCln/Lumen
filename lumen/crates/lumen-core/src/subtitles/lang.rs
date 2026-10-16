//! Language codes. The player's preferences use ISO 639-1 (`en`); subtitle services use the
//! three-letter codes of ISO 639-2 (`eng`, `fre`, `ger`) or English names, so each lookup needs a
//! way across.

struct Language {
    /// ISO 639-1, what Lumen stores and the UI offers.
    code: &'static str,
    /// ISO 639-2/B, which OpenSubtitles-derived services answer with.
    bibliographic: &'static str,
    /// What Addic7ed-derived services call it.
    name: &'static str,
    /// Other spellings seen in the wild (ISO 639-2/T, regional variants).
    aliases: &'static [&'static str],
}

const LANGUAGES: &[Language] = &[
    Language {
        code: "en",
        bibliographic: "eng",
        name: "English",
        aliases: &[],
    },
    Language {
        code: "es",
        bibliographic: "spa",
        name: "Spanish",
        aliases: &["spn", "spl"],
    },
    Language {
        code: "fr",
        bibliographic: "fre",
        name: "French",
        aliases: &["fra"],
    },
    Language {
        code: "de",
        bibliographic: "ger",
        name: "German",
        aliases: &["deu"],
    },
    Language {
        code: "it",
        bibliographic: "ita",
        name: "Italian",
        aliases: &[],
    },
    Language {
        code: "pt",
        bibliographic: "por",
        name: "Portuguese",
        aliases: &["pob", "pb", "pt-br"],
    },
    Language {
        code: "nl",
        bibliographic: "dut",
        name: "Dutch",
        aliases: &["nld"],
    },
    Language {
        code: "sv",
        bibliographic: "swe",
        name: "Swedish",
        aliases: &[],
    },
    Language {
        code: "no",
        bibliographic: "nor",
        name: "Norwegian",
        aliases: &["nob"],
    },
    Language {
        code: "da",
        bibliographic: "dan",
        name: "Danish",
        aliases: &[],
    },
    Language {
        code: "fi",
        bibliographic: "fin",
        name: "Finnish",
        aliases: &[],
    },
    Language {
        code: "pl",
        bibliographic: "pol",
        name: "Polish",
        aliases: &[],
    },
    Language {
        code: "cs",
        bibliographic: "cze",
        name: "Czech",
        aliases: &["ces"],
    },
    Language {
        code: "hu",
        bibliographic: "hun",
        name: "Hungarian",
        aliases: &[],
    },
    Language {
        code: "ro",
        bibliographic: "rum",
        name: "Romanian",
        aliases: &["ron"],
    },
    Language {
        code: "el",
        bibliographic: "ell",
        name: "Greek",
        aliases: &["gre"],
    },
    Language {
        code: "tr",
        bibliographic: "tur",
        name: "Turkish",
        aliases: &[],
    },
    Language {
        code: "ru",
        bibliographic: "rus",
        name: "Russian",
        aliases: &[],
    },
    Language {
        code: "uk",
        bibliographic: "ukr",
        name: "Ukrainian",
        aliases: &[],
    },
    Language {
        code: "ar",
        bibliographic: "ara",
        name: "Arabic",
        aliases: &[],
    },
    Language {
        code: "he",
        bibliographic: "heb",
        name: "Hebrew",
        aliases: &[],
    },
    Language {
        code: "hi",
        bibliographic: "hin",
        name: "Hindi",
        aliases: &[],
    },
    Language {
        code: "th",
        bibliographic: "tha",
        name: "Thai",
        aliases: &[],
    },
    Language {
        code: "vi",
        bibliographic: "vie",
        name: "Vietnamese",
        aliases: &[],
    },
    Language {
        code: "id",
        bibliographic: "ind",
        name: "Indonesian",
        aliases: &[],
    },
    Language {
        code: "zh",
        bibliographic: "chi",
        name: "Chinese",
        aliases: &["zho", "zht", "zhc", "zh-cn", "zh-tw"],
    },
    Language {
        code: "ja",
        bibliographic: "jpn",
        name: "Japanese",
        aliases: &[],
    },
    Language {
        code: "ko",
        bibliographic: "kor",
        name: "Korean",
        aliases: &[],
    },
];

fn find(code: &str) -> Option<&'static Language> {
    let wanted = code.trim().to_ascii_lowercase();
    LANGUAGES.iter().find(|language| {
        language.code == wanted
            || language.bibliographic == wanted
            || language.aliases.contains(&wanted.as_str())
    })
}

/// The ISO 639-1 code for any spelling in the table (`fre`, `pob`, `EN`), or `None` for a language
/// Lumen does not offer.
pub fn normalize(code: &str) -> Option<&'static str> {
    find(code).map(|language| language.code)
}

/// The ISO 639-2/B code (`fre`) for a Lumen language code.
pub fn bibliographic(code: &str) -> Option<&'static str> {
    find(code).map(|language| language.bibliographic)
}

/// The English name (`French`) for a Lumen language code.
pub fn english_name(code: &str) -> Option<&'static str> {
    find(code).map(|language| language.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_maps_to_one_code() {
        for (spelling, code) in [
            ("en", "en"),
            ("ENG", "en"),
            ("fre", "fr"),
            ("fra", "fr"),
            ("ger", "de"),
            ("pob", "pt"),
            ("pt-BR", "pt"),
            ("zht", "zh"),
            ("ell", "el"),
            (" cze ", "cs"),
        ] {
            assert_eq!(normalize(spelling), Some(code), "{spelling}");
        }
        assert_eq!(normalize("xx"), None);
        assert_eq!(normalize(""), None);
    }

    #[test]
    fn codes_and_names_round_trip() {
        assert_eq!(bibliographic("fr"), Some("fre"));
        assert_eq!(english_name("ja"), Some("Japanese"));
        assert_eq!(bibliographic("xx"), None);
        // Every row must be reachable from its own three spellings, and no two rows may collide.
        for language in LANGUAGES {
            assert_eq!(normalize(language.code), Some(language.code));
            assert_eq!(normalize(language.bibliographic), Some(language.code));
            for alias in language.aliases {
                assert_eq!(normalize(alias), Some(language.code), "{alias}");
            }
        }
    }
}

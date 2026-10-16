-- Subtitle lookup.
--
-- imdb_ids: the IMDb id found for a title (by its TMDB id, or by name and year), so each title is
-- looked up once. Not synced: it is a cache and can be found again.
CREATE TABLE IF NOT EXISTS imdb_ids (
    lookup_key TEXT PRIMARY KEY,
    imdb_id TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

-- subtitle_choices: the subtitle the user picked for a title in a language, as a file in this
-- device's cache. Replaying the title loads it again with no network. Not synced: the path only
-- means something on this device.
CREATE TABLE IF NOT EXISTS subtitle_choices (
    content_key TEXT NOT NULL,
    language TEXT NOT NULL,
    path TEXT NOT NULL,
    provider TEXT NOT NULL,
    release TEXT NOT NULL DEFAULT '',
    chosen_at INTEGER NOT NULL,
    PRIMARY KEY (content_key, language)
);

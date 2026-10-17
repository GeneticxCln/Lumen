-- Manual Kind/Genre correction for a title TMDB never matched.
--
-- Keyed by content_key (see content_key.rs), the same stable identity favorites and history use,
-- so a correction survives re-imports and applies to every playlist row that derives the same
-- key. Once a title later gets a real TMDB match, TMDB stays authoritative and any leftover row
-- here is simply ignored by readers.
CREATE TABLE IF NOT EXISTS category_overrides (
    content_key TEXT PRIMARY KEY,
    kind TEXT,                           -- 'movie' | 'series' | 'live'; NULL when only genres were set
    genres TEXT,                         -- semicolon-joined, same format as channels.genre_names
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

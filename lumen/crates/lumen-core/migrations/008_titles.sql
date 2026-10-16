-- Title identity and source aggregation (Stremio model).
-- Titles exist independently of individual stream URLs, identified by a stable content_key:
--   - tmdb:movie:<id>
--   - tmdb:tv:<id>:S<season>E<episode>
--   - tvg:<tvg_id>
--   - file:<fingerprint>
--   - stream:<sha256_of_base_url> (fallback when no canonical metadata exists)

CREATE TABLE IF NOT EXISTS titles (
    content_key TEXT PRIMARY KEY,
    kind TEXT NOT NULL,                  -- 'movie' | 'series' | 'channel' | 'local'
    title TEXT NOT NULL,
    tmdb_id INTEGER,
    tmdb_type TEXT,
    season INTEGER,
    episode INTEGER,
    release_year INTEGER,
    poster_path TEXT,
    backdrop_path TEXT,
    overview TEXT,
    genres TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_titles_tmdb ON titles(tmdb_id, tmdb_type);
CREATE INDEX IF NOT EXISTS idx_titles_kind ON titles(kind);

-- Ranked sources that can play a title. Resolves stream URLs at play time.
CREATE TABLE IF NOT EXISTS title_sources (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content_key TEXT NOT NULL REFERENCES titles(content_key) ON DELETE CASCADE,
    channel_id INTEGER REFERENCES channels(id) ON DELETE CASCADE,
    stream_url TEXT NOT NULL,
    rank INTEGER NOT NULL DEFAULT 0,     -- higher rank = preferred source
    quality TEXT,
    headers TEXT DEFAULT '{}',
    is_live INTEGER,
    last_verified INTEGER,
    created_at INTEGER NOT NULL,
    UNIQUE(content_key, stream_url)
);

CREATE INDEX IF NOT EXISTS idx_title_sources_key_rank ON title_sources(content_key, rank DESC);
CREATE INDEX IF NOT EXISTS idx_title_sources_channel ON title_sources(channel_id);

-- Add content_key reference to favorites and history for stable resume across streams
ALTER TABLE favorites ADD COLUMN content_key TEXT;
CREATE INDEX IF NOT EXISTS idx_favorites_content_key ON favorites(profile_id, content_key);

ALTER TABLE history ADD COLUMN content_key TEXT;
CREATE INDEX IF NOT EXISTS idx_history_content_key ON history(profile_id, content_key);

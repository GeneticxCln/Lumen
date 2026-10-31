-- Derived browse facets, one row per channel plus one row per distinct group/genre.
-- These are maintained by the core crate rather than computed in the UI, so counts and filters
-- cover the whole library instead of whichever 500-row page happens to be loaded.
CREATE TABLE IF NOT EXISTS channel_facets (
    channel_id INTEGER PRIMARY KEY REFERENCES channels(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,                  -- 'movie' | 'series' | 'live'
    adult INTEGER NOT NULL DEFAULT 0,
    year TEXT NOT NULL                   -- four-digit year or 'unknown'
);

CREATE TABLE IF NOT EXISTS channel_facet_groups (
    channel_id INTEGER NOT NULL REFERENCES channel_facets(channel_id) ON DELETE CASCADE,
    value TEXT NOT NULL,
    PRIMARY KEY (channel_id, value)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS channel_facet_genres (
    channel_id INTEGER NOT NULL REFERENCES channel_facets(channel_id) ON DELETE CASCADE,
    value TEXT NOT NULL,
    PRIMARY KEY (channel_id, value)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_channel_facets_kind ON channel_facets(kind);
CREATE INDEX IF NOT EXISTS idx_channel_facets_year ON channel_facets(year);
CREATE INDEX IF NOT EXISTS idx_channel_facet_groups_value ON channel_facet_groups(value, channel_id);
CREATE INDEX IF NOT EXISTS idx_channel_facet_genres_value ON channel_facet_genres(value, channel_id);

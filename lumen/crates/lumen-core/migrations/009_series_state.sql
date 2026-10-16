-- Series state.
--
-- `episode_state` holds the watched marks a person sets by hand ("mark watched / unwatched"). It
-- overrides what history would say, and `watched = 0` is a real answer ("I have not seen this"),
-- not a deletion. `series_key` is opaque to the database: `tmdb:tv:<id>` when the title is matched
-- on TMDB, otherwise `name:<normalised series name>`. `updated_at` (ms) decides last-write-wins
-- across synced devices.
CREATE TABLE IF NOT EXISTS episode_state (
    profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    series_key TEXT NOT NULL,
    season INTEGER NOT NULL,
    episode INTEGER NOT NULL,
    watched INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (profile_id, series_key, season, episode)
);

-- What TMDB says about a series' schedule, for "coming up" and "new episode" notices.
CREATE TABLE IF NOT EXISTS series_airing (
    tmdb_id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    status TEXT,
    next_air_date TEXT,
    next_season INTEGER,
    next_episode INTEGER,
    next_name TEXT,
    last_air_date TEXT,
    last_season INTEGER,
    last_episode INTEGER,
    checked_at INTEGER NOT NULL
);

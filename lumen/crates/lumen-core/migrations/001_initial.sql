CREATE TABLE playlists (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    source TEXT NOT NULL,
    origin TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE channels (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    url TEXT NOT NULL,
    group_title TEXT,
    tvg_id TEXT,
    tvg_logo TEXT,
    extra_headers TEXT DEFAULT '{}',
    last_ok INTEGER,
    last_error TEXT
);

CREATE INDEX idx_channels_playlist_id ON channels(playlist_id);
CREATE INDEX idx_channels_group_title ON channels(playlist_id, group_title);
CREATE INDEX idx_channels_name ON channels(name);

CREATE TABLE favorites (
    channel_id INTEGER PRIMARY KEY REFERENCES channels(id) ON DELETE CASCADE
);

CREATE TABLE history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id INTEGER REFERENCES channels(id) ON DELETE SET NULL,
    path TEXT NOT NULL,
    position REAL NOT NULL,
    duration REAL NOT NULL,
    played_at INTEGER NOT NULL
);

CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
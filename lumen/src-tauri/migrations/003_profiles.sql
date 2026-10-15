-- On-device profiles. Lumen has no server and no online accounts: a profile is a local
-- identity that scopes watch history and My List. `pin_hash` is an Argon2id hash of an
-- optional PIN; it locks that profile's UI on this device and is not encryption of the
-- library, the database or the media files.
CREATE TABLE profiles (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    color TEXT,
    pin_hash TEXT,
    created_at INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL
);

-- Everything the current install already holds belongs to this first profile.
INSERT INTO profiles (id, name, color, pin_hash, created_at, last_used_at)
VALUES (1, 'Lumen', 'accent', NULL, CAST(strftime('%s', 'now') AS INTEGER), CAST(strftime('%s', 'now') AS INTEGER));

-- `history.profile_id` must be NOT NULL, and SQLite cannot add a NOT NULL foreign-key
-- column to an existing table, so the table is rebuilt with the existing rows assigned to
-- the profile above. Rebuilding also drops the old single-profile indexes, which are
-- recreated below keyed by profile.
CREATE TABLE history_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    channel_id INTEGER REFERENCES channels(id) ON DELETE SET NULL,
    path TEXT NOT NULL,
    position REAL NOT NULL,
    duration REAL NOT NULL,
    played_at INTEGER NOT NULL
);

INSERT INTO history_new (id, profile_id, channel_id, path, position, duration, played_at)
SELECT id, 1, channel_id, path, position, duration, played_at FROM history;

DROP TABLE history;
ALTER TABLE history_new RENAME TO history;

-- My List is per profile, so the primary key becomes (profile_id, channel_id). Existing
-- favorites belong to the first profile.
CREATE TABLE favorites_new (
    profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    PRIMARY KEY (profile_id, channel_id)
);

INSERT INTO favorites_new (profile_id, channel_id) SELECT 1, channel_id FROM favorites;

DROP TABLE favorites;
ALTER TABLE favorites_new RENAME TO favorites;

CREATE INDEX idx_history_profile_path ON history(profile_id, path, played_at);
CREATE INDEX idx_history_profile_played_at ON history(profile_id, played_at);

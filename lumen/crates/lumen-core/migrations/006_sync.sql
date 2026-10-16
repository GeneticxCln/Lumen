-- Favorites are keyed by the stream URL, not the local channel id. Channel ids change when a
-- playlist is re-imported and differ between devices, so the URL is the only stable identity.
-- `active = 0` is a tombstone (un-favorited or its playlist was deleted) so that a sync can
-- tell "removed" from "never set". `updated_at` (ms) decides last-write-wins.
CREATE TABLE favorites_new (
    profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    channel_url TEXT NOT NULL,
    active INTEGER NOT NULL DEFAULT 1,
    updated_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (profile_id, channel_url)
);

INSERT INTO favorites_new (profile_id, channel_url, active, updated_at)
SELECT f.profile_id, c.url, 1, CAST(strftime('%s', 'now') AS INTEGER) * 1000
FROM favorites f
JOIN channels c ON c.id = f.channel_id
GROUP BY f.profile_id, c.url;

DROP TABLE favorites;
ALTER TABLE favorites_new RENAME TO favorites;

-- Lookups from a favorite's URL back to its channels.
CREATE INDEX IF NOT EXISTS idx_channels_url ON channels(url);

-- Sync identity: random per profile and the same on every device that signs in. Profiles
-- created before this migration get one now. `updated_at` (ms) decides last-write-wins.
ALTER TABLE profiles ADD COLUMN sync_key TEXT;
ALTER TABLE profiles ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
UPDATE profiles SET sync_key = lower(hex(randomblob(16))) WHERE sync_key IS NULL;
UPDATE profiles SET updated_at = created_at * 1000 WHERE updated_at = 0;
CREATE UNIQUE INDEX idx_profiles_sync_key ON profiles(sync_key);

-- Changes waiting to be pushed to the account. Only the latest state of each record is kept,
-- so a burst of progress saves becomes one row. Rows are removed once the server accepts them.
CREATE TABLE sync_queue (
    kind TEXT NOT NULL,
    item_key TEXT NOT NULL,
    payload TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (kind, item_key)
);

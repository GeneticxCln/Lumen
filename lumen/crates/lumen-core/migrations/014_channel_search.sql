-- Full-text search over the library. The index is external-content, so it stores only the
-- inverted index and reads the row back from `channels`. Triggers keep it current for imports,
-- refreshes and deletes. Existing libraries are indexed separately by the resumable
-- `backfill_channel_search` job; search falls back to LIKE until that job records completion.
CREATE VIRTUAL TABLE IF NOT EXISTS channels_fts USING fts5(
    name,
    group_title,
    tvg_id,
    content='channels',
    content_rowid='id',
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER IF NOT EXISTS channels_fts_ai AFTER INSERT ON channels BEGIN
    INSERT INTO channels_fts(rowid, name, group_title, tvg_id)
    VALUES (new.id, new.name, new.group_title, new.tvg_id);
END;

CREATE TRIGGER IF NOT EXISTS channels_fts_ad AFTER DELETE ON channels BEGIN
    INSERT INTO channels_fts(channels_fts, rowid, name, group_title, tvg_id)
    VALUES ('delete', old.id, old.name, old.group_title, old.tvg_id);
END;

CREATE TRIGGER IF NOT EXISTS channels_fts_au AFTER UPDATE OF name, group_title, tvg_id ON channels BEGIN
    INSERT INTO channels_fts(channels_fts, rowid, name, group_title, tvg_id)
    VALUES ('delete', old.id, old.name, old.group_title, old.tvg_id);
    INSERT INTO channels_fts(rowid, name, group_title, tvg_id)
    VALUES (new.id, new.name, new.group_title, new.tvg_id);
END;

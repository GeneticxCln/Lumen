-- A playlist refresh rewrites every matched channel, usually with identical text. The old trigger
-- deleted and re-inserted each row's index entry anyway; only a real change to an indexed column
-- needs to touch the full-text index.
DROP TRIGGER IF EXISTS channels_fts_au;

CREATE TRIGGER channels_fts_au AFTER UPDATE OF name, group_title, tvg_id ON channels
WHEN old.name IS NOT new.name OR old.group_title IS NOT new.group_title OR old.tvg_id IS NOT new.tvg_id
BEGIN
    INSERT INTO channels_fts(channels_fts, rowid, name, group_title, tvg_id)
    VALUES ('delete', old.id, old.name, old.group_title, old.tvg_id);
    INSERT INTO channels_fts(rowid, name, group_title, tvg_id)
    VALUES (new.id, new.name, new.group_title, new.tvg_id);
END;

-- Sources that outlived their channel row are looked up by stream URL; the index holds only
-- those (none, in a normal library), so it costs nothing to build or keep.
CREATE INDEX IF NOT EXISTS idx_title_sources_orphan_url ON title_sources(stream_url) WHERE channel_id IS NULL;

-- History is read as "most recent entry per path" (resume points, recently watched), so the
-- lookups in db.rs key on (path, played_at). Rows are added and compacted in one transaction,
-- which keeps this migration non-destructive: existing duplicate rows are collapsed on read
-- and pruned at startup rather than deleted here.
CREATE INDEX idx_history_path_played_at ON history(path, played_at);

CREATE INDEX idx_history_played_at ON history(played_at);

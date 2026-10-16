-- Per-file poster frame choices, keyed by canonical path. A file without a row follows the
-- global `poster_frame_seconds` setting. Created idempotently to match the bridge's schema.
CREATE TABLE IF NOT EXISTS poster_frames (
    path TEXT PRIMARY KEY,
    seconds INTEGER NOT NULL
);

-- Language label from the playlist's `tvg-language` attribute. NULL when the playlist does not
-- declare one; existing rows stay NULL until their playlist is imported again.
ALTER TABLE channels ADD COLUMN language TEXT;

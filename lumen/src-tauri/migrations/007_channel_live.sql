-- Whether the playlist marked an entry live (#EXTINF duration -1) or on-demand (a positive
-- duration). NULL when the list gave no usable duration; existing rows stay NULL until their
-- playlist is imported again.
ALTER TABLE channels ADD COLUMN is_live INTEGER;

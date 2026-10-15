// D1 schema, applied by the Worker once per isolate (Sites has no SQL console).
// Every statement is idempotent. Only hashes of codes and tokens are stored.
const SCHEMA_STATEMENTS = [
  `CREATE TABLE IF NOT EXISTS desktop_codes (
    code_hash TEXT PRIMARY KEY,
    sub TEXT NOT NULL,
    redirect_uri TEXT NOT NULL,
    code_challenge TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    used INTEGER NOT NULL DEFAULT 0
  )`,
  `CREATE TABLE IF NOT EXISTS app_sessions (
    token_hash TEXT PRIMARY KEY,
    sub TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    revoked INTEGER NOT NULL DEFAULT 0
  )`,
  `CREATE INDEX IF NOT EXISTS app_sessions_sub ON app_sessions (sub)`,
  // One row per synced record. Last write wins on updated_at (client clock, ms).
  `CREATE TABLE IF NOT EXISTS sync_items (
    sub TEXT NOT NULL,
    kind TEXT NOT NULL,
    item_key TEXT NOT NULL,
    payload TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (sub, kind, item_key)
  )`,
  `CREATE INDEX IF NOT EXISTS sync_items_pull ON sync_items (sub, updated_at)`,
]

export function applySchema(db: D1Database): Promise<unknown> {
  return db.batch(SCHEMA_STATEMENTS.map((sql) => db.prepare(sql)))
}

// Sync server for Lumen. There are no accounts and no logins.
//
// A device creates a random secret and derives from it a public group id and an authentication
// token (see src-tauri/src/sync_crypto.rs). It registers the group with SHA-256 of the token, then
// stores encrypted records under opaque ids. This server never holds a key that can decrypt them:
// it sees the group id, opaque item ids, timestamps, a deleted flag and blob sizes, and nothing
// about what the records are.
//
// With no identity to rate-limit, abuse is bounded by a proof of work to create a group, a
// per-IP daily cap on creations, a row cap per group, and expiry of idle groups.

export interface Env {
  DB: D1Database
  /** Leading zero bits required to create a group. Default 20 (about a million hashes). */
  POW_BITS?: string
  /** Groups one IP address may create per day. Default 20. */
  GROUP_CREATE_LIMIT?: string
  /** Rows one group may hold. Default 20000. */
  MAX_ITEMS?: string
  /** Mixed into the hashed IP used for the daily creation cap, so the hash cannot be reversed. */
  RATE_SALT?: string
}

const DAY_MS = 24 * 60 * 60 * 1000
const GROUP_IDLE_EXPIRY_MS = 180 * DAY_MS
const TOMBSTONE_KEEP_MS = 90 * DAY_MS
const TOUCH_INTERVAL_MS = 60 * 60 * 1000
const MAX_PUSH = 500
const MAX_PULL = 500
const MAX_BLOB_CHARS = 6144
const MAX_BODY_BYTES = 4 * 1024 * 1024
const NO_STORE = { 'Cache-Control': 'private, no-store' }

const SYNC_ID_RE = /^[0-9a-f]{32}$/
const ITEM_ID_RE = /^[0-9a-f]{32}$/
const VERIFIER_RE = /^[0-9a-f]{64}$/
const TOKEN_RE = /^Bearer ([A-Za-z0-9_-]{43})$/
const BLOB_RE = /^[A-Za-z0-9_-]+$/

function json(body: unknown, status = 200): Response {
  return Response.json(body, { status, headers: NO_STORE })
}

function intVar(value: string | undefined, fallback: number): number {
  if (value === undefined || value === '') return fallback
  const parsed = Number(value)
  return Number.isSafeInteger(parsed) && parsed >= 0 ? parsed : fallback
}

async function sha256Bytes(input: string): Promise<Uint8Array> {
  return new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(input)))
}

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')
}

/** Compares two strings without stopping at the first difference. */
function constantTimeEqual(a: string, b: string): boolean {
  if (a.length !== b.length) return false
  let difference = 0
  for (let index = 0; index < a.length; index++) difference |= a.charCodeAt(index) ^ b.charCodeAt(index)
  return difference === 0
}

function leadingZeroBits(hash: Uint8Array): number {
  let bits = 0
  for (const byte of hash) {
    if (byte === 0) {
      bits += 8
      continue
    }
    return bits + Math.clz32(byte) - 24
  }
  return bits
}

async function readJson(request: Request): Promise<unknown | null> {
  const declared = Number(request.headers.get('Content-Length') ?? '0')
  if (declared > MAX_BODY_BYTES) return null
  try {
    const text = await request.text()
    if (text.length > MAX_BODY_BYTES) return null
    return JSON.parse(text)
  } catch {
    return null
  }
}

/** The group this request is for, once its token checks out; otherwise the response to send. */
async function authenticate(request: Request, env: Env): Promise<{ syncId: string } | Response> {
  const syncId = request.headers.get('X-Sync-Id') ?? ''
  const match = TOKEN_RE.exec(request.headers.get('Authorization') ?? '')
  if (!SYNC_ID_RE.test(syncId) || !match) return json({ error: 'unauthorized' }, 401)

  const row = await env.DB.prepare('SELECT verifier, last_seen_at FROM sync_groups WHERE sync_id = ?')
    .bind(syncId)
    .first<{ verifier: string; last_seen_at: number }>()
  // A sync id is 128 random bits, so telling "no such group" apart from "wrong token" leaks
  // nothing guessable and lets a device with a valid key say so.
  if (!row) return json({ error: 'group_not_found' }, 404)
  if (!constantTimeEqual(toHex(await sha256Bytes(match[1])), row.verifier)) return json({ error: 'unauthorized' }, 401)

  const now = Date.now()
  if (now - row.last_seen_at > TOUCH_INTERVAL_MS) {
    await env.DB.prepare('UPDATE sync_groups SET last_seen_at = ? WHERE sync_id = ?').bind(now, syncId).run()
  }
  return { syncId }
}

async function createGroup(request: Request, env: Env): Promise<Response> {
  const body = (await readJson(request)) as { sync_id?: unknown; verifier?: unknown; pow_nonce?: unknown } | null
  if (!body || typeof body !== 'object') return json({ error: 'invalid_request' }, 400)
  const { sync_id: syncId, verifier, pow_nonce: nonce } = body
  if (
    typeof syncId !== 'string' ||
    !SYNC_ID_RE.test(syncId) ||
    typeof verifier !== 'string' ||
    !VERIFIER_RE.test(verifier) ||
    typeof nonce !== 'number' ||
    !Number.isSafeInteger(nonce) ||
    nonce < 0
  ) {
    return json({ error: 'invalid_request' }, 400)
  }

  // Registering the same group again is how a second run of "turn on sync" recovers; a different
  // verifier for an existing id is someone else's group.
  const existing = await env.DB.prepare('SELECT verifier FROM sync_groups WHERE sync_id = ?')
    .bind(syncId)
    .first<{ verifier: string }>()
  if (existing) {
    return constantTimeEqual(existing.verifier, verifier) ? json({ created: false }) : json({ error: 'conflict' }, 409)
  }

  const bits = intVar(env.POW_BITS, 20)
  if (leadingZeroBits(await sha256Bytes(`${syncId}:${nonce}`)) < bits) return json({ error: 'invalid_pow' }, 400)

  const now = Date.now()
  const day = Math.floor(now / DAY_MS)
  const ip = request.headers.get('cf-connecting-ip') ?? 'unknown'
  const ipHash = toHex(await sha256Bytes(`${env.RATE_SALT ?? 'lumen'}:${day}:${ip}`)).slice(0, 32)
  await env.DB.prepare(
    `INSERT INTO group_creations (day, ip_hash, n) VALUES (?, ?, 1)
     ON CONFLICT (day, ip_hash) DO UPDATE SET n = n + 1`,
  )
    .bind(day, ipHash)
    .run()
  const counted = await env.DB.prepare('SELECT n FROM group_creations WHERE day = ? AND ip_hash = ?')
    .bind(day, ipHash)
    .first<{ n: number }>()
  if ((counted?.n ?? 0) > intVar(env.GROUP_CREATE_LIMIT, 20)) return json({ error: 'rate_limited' }, 429)

  // Clear what is no longer needed while writing anyway: idle groups and old counters.
  const cutoff = now - GROUP_IDLE_EXPIRY_MS
  await env.DB.batch([
    env.DB.prepare('DELETE FROM sync_blobs WHERE sync_id IN (SELECT sync_id FROM sync_groups WHERE last_seen_at < ?)').bind(cutoff),
    env.DB.prepare('DELETE FROM sync_seq WHERE sync_id IN (SELECT sync_id FROM sync_groups WHERE last_seen_at < ?)').bind(cutoff),
    env.DB.prepare('DELETE FROM sync_groups WHERE last_seen_at < ?').bind(cutoff),
    env.DB.prepare('DELETE FROM group_creations WHERE day < ?').bind(day - 2),
    env.DB.prepare('INSERT INTO sync_groups (sync_id, verifier, created_at, last_seen_at) VALUES (?, ?, ?, ?)').bind(
      syncId,
      verifier,
      now,
      now,
    ),
    env.DB.prepare('INSERT INTO sync_seq (sync_id, value) VALUES (?, 0) ON CONFLICT (sync_id) DO NOTHING').bind(syncId),
  ])
  return json({ created: true }, 201)
}

async function deleteGroup(request: Request, env: Env): Promise<Response> {
  const auth = await authenticate(request, env)
  if (auth instanceof Response) return auth
  await env.DB.batch([
    env.DB.prepare('DELETE FROM sync_blobs WHERE sync_id = ?').bind(auth.syncId),
    env.DB.prepare('DELETE FROM sync_seq WHERE sync_id = ?').bind(auth.syncId),
    env.DB.prepare('DELETE FROM sync_groups WHERE sync_id = ?').bind(auth.syncId),
  ])
  return json({ ok: true })
}

interface PushItem {
  id: string
  updated_at: number
  deleted: boolean
  blob: string
}

function parsePush(value: unknown): PushItem[] | null {
  if (!value || typeof value !== 'object' || !Array.isArray((value as { items?: unknown }).items)) return null
  const items = (value as { items: unknown[] }).items
  if (items.length > MAX_PUSH) return null
  const parsed: PushItem[] = []
  for (const item of items) {
    if (!item || typeof item !== 'object') return null
    const { id, updated_at: updatedAt, deleted, blob } = item as Record<string, unknown>
    if (
      typeof id !== 'string' ||
      !ITEM_ID_RE.test(id) ||
      typeof updatedAt !== 'number' ||
      !Number.isSafeInteger(updatedAt) ||
      updatedAt < 0 ||
      typeof deleted !== 'boolean' ||
      typeof blob !== 'string' ||
      blob.length === 0 ||
      blob.length > MAX_BLOB_CHARS ||
      !BLOB_RE.test(blob)
    ) {
      return null
    }
    parsed.push({ id, updated_at: updatedAt, deleted, blob })
  }
  return parsed
}

/** D1 rejects a statement with too many bound parameters well below SQLite's own limit. */
const MAX_BOUND_ITEM_IDS_PER_QUERY = 50

/** How many of these item ids this group already holds, chunked to stay under D1's bind-parameter cap. */
async function countAlreadyHeld(env: Env, syncId: string, itemIds: string[]): Promise<number> {
  let count = 0
  for (let start = 0; start < itemIds.length; start += MAX_BOUND_ITEM_IDS_PER_QUERY) {
    const chunk = itemIds.slice(start, start + MAX_BOUND_ITEM_IDS_PER_QUERY)
    const placeholders = chunk.map(() => '?').join(',')
    const result = await env.DB.prepare(
      `SELECT COUNT(*) AS n FROM sync_blobs WHERE sync_id = ? AND item_id IN (${placeholders})`,
    )
      .bind(syncId, ...chunk)
      .first<{ n: number }>()
    count += result?.n ?? 0
  }
  return count
}

async function pushItems(request: Request, env: Env): Promise<Response> {
  const auth = await authenticate(request, env)
  if (auth instanceof Response) return auth
  const items = parsePush(await readJson(request))
  if (!items) return json({ error: 'invalid_request' }, 400)
  if (items.length === 0) return json({ ok: true, received: 0 })

  // Old deletion markers go first, so the cap never traps a group that holds mostly tombstones.
  await env.DB.prepare('DELETE FROM sync_blobs WHERE sync_id = ? AND deleted = 1 AND updated_at < ?')
    .bind(auth.syncId, Date.now() - TOMBSTONE_KEEP_MS)
    .run()
  const held = await env.DB.prepare('SELECT COUNT(*) AS n FROM sync_blobs WHERE sync_id = ?')
    .bind(auth.syncId)
    .first<{ n: number }>()
  // The write below is an upsert, so a pushed item already held adds no new row. Only the ids
  // this group does not yet hold should count against the cap — otherwise an actively re-synced,
  // near-full group (mostly re-pushes of items it already has) gets rejected well below its real
  // row count.
  const newRows = items.length - (await countAlreadyHeld(env, auth.syncId, items.map((item) => item.id)))
  if ((held?.n ?? 0) + newRows > intVar(env.MAX_ITEMS, 20_000)) return json({ error: 'quota_exceeded' }, 413)

  // A server-assigned, monotonically increasing sequence, independent of the client-supplied
  // `updated_at`: the pull cursor pages by this instead, so a device that re-uploads its own
  // older-timestamped data (joining a group late, or an edit made long offline) is still seen as
  // new by every other device's cursor, which can only ever be behind it, never past it.
  // `createGroup` seeds this row for new groups; the seed here is just a safety net for groups
  // created before this column existed.
  await env.DB.prepare('INSERT INTO sync_seq (sync_id, value) VALUES (?, 0) ON CONFLICT (sync_id) DO NOTHING')
    .bind(auth.syncId)
    .run()
  const seqRow = await env.DB.prepare('UPDATE sync_seq SET value = value + 1 WHERE sync_id = ? RETURNING value')
    .bind(auth.syncId)
    .first<{ value: number }>()
  const seq = seqRow?.value ?? Date.now()

  // Last write wins: a stale push never replaces a newer record. `seq` advances on every push that
  // lands a write, whether that write is a brand-new item or a genuinely newer update to one.
  await env.DB.batch(
    items.map((item) =>
      env.DB.prepare(
        `INSERT INTO sync_blobs (sync_id, item_id, updated_at, deleted, blob, seq) VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (sync_id, item_id) DO UPDATE SET
           updated_at = excluded.updated_at,
           deleted = excluded.deleted,
           blob = excluded.blob,
           seq = excluded.seq
         WHERE excluded.updated_at > sync_blobs.updated_at`,
      ).bind(auth.syncId, item.id, item.updated_at, item.deleted ? 1 : 0, item.blob, seq),
    ),
  )
  return json({ ok: true, received: items.length })
}

async function pullItems(request: Request, env: Env): Promise<Response> {
  const auth = await authenticate(request, env)
  if (auth instanceof Response) return auth
  const params = new URL(request.url).searchParams
  const afterId = params.get('after_id')
  if (afterId !== null && !ITEM_ID_RE.test(afterId)) return json({ error: 'invalid_request' }, 400)

  // `since_seq`, present (even as "0"): a client that knows about the server-assigned sequence.
  // Paging by `seq` instead of `updated_at` is what fixes pre-existing-data loss (see pushItems)
  // - a record pushed with an old `updated_at` still has a `seq` newer than any cursor already
  // past that timestamp. Absent: an old client, unaware of `since_seq`, kept on its original
  // `updated_at`-based paging exactly as before - bug included - until it updates.
  if (params.has('since_seq')) {
    const sinceSeq = Number(params.get('since_seq'))
    if (!Number.isSafeInteger(sinceSeq) || sinceSeq < 0) return json({ error: 'invalid_request' }, 400)
    const statement =
      afterId !== null
        ? env.DB.prepare(
            `SELECT item_id, updated_at, deleted, blob, seq FROM sync_blobs
             WHERE sync_id = ? AND (seq > ? OR (seq = ? AND item_id > ?))
             ORDER BY seq, item_id LIMIT ?`,
          ).bind(auth.syncId, sinceSeq, sinceSeq, afterId, MAX_PULL + 1)
        : env.DB.prepare(
            `SELECT item_id, updated_at, deleted, blob, seq FROM sync_blobs
             WHERE sync_id = ? AND seq > ? ORDER BY seq, item_id LIMIT ?`,
          ).bind(auth.syncId, sinceSeq, MAX_PULL + 1)
    const rows =
      (await statement.all<{ item_id: string; updated_at: number; deleted: number; blob: string; seq: number }>()).results ?? []
    return json({
      items: rows.slice(0, MAX_PULL).map((row) => ({
        id: row.item_id,
        updated_at: row.updated_at,
        deleted: row.deleted === 1,
        blob: row.blob,
        seq: row.seq,
      })),
      has_more: rows.length > MAX_PULL,
    })
  }

  const since = Number(params.get('since') ?? '0')
  if (!Number.isSafeInteger(since) || since < 0) return json({ error: 'invalid_request' }, 400)

  // Pages run in (updated_at, item_id) order. A page can end in the middle of records that share
  // a timestamp, so the next request names the last record it received and resumes after it.
  const statement =
    afterId !== null
      ? env.DB.prepare(
          `SELECT item_id, updated_at, deleted, blob FROM sync_blobs
           WHERE sync_id = ? AND (updated_at > ? OR (updated_at = ? AND item_id > ?))
           ORDER BY updated_at, item_id LIMIT ?`,
        ).bind(auth.syncId, since, since, afterId, MAX_PULL + 1)
      : env.DB.prepare(
          `SELECT item_id, updated_at, deleted, blob FROM sync_blobs
           WHERE sync_id = ? AND updated_at > ? ORDER BY updated_at, item_id LIMIT ?`,
        ).bind(auth.syncId, since, MAX_PULL + 1)
  const rows = (await statement.all<{ item_id: string; updated_at: number; deleted: number; blob: string }>()).results ?? []
  return json({
    items: rows.slice(0, MAX_PULL).map((row) => ({
      id: row.item_id,
      updated_at: row.updated_at,
      deleted: row.deleted === 1,
      blob: row.blob,
    })),
    has_more: rows.length > MAX_PULL,
  })
}

const SCHEMA = [
  `CREATE TABLE IF NOT EXISTS sync_groups (
    sync_id TEXT PRIMARY KEY,
    verifier TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL
  )`,
  `CREATE TABLE IF NOT EXISTS sync_blobs (
    sync_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0,
    blob TEXT NOT NULL,
    PRIMARY KEY (sync_id, item_id)
  )`,
  // Kept for legacy clients, which still pull in (updated_at, item_id) order - see pullItems.
  `CREATE INDEX IF NOT EXISTS sync_blobs_pull ON sync_blobs (sync_id, updated_at, item_id)`,
  `CREATE TABLE IF NOT EXISTS group_creations (
    day INTEGER NOT NULL,
    ip_hash TEXT NOT NULL,
    n INTEGER NOT NULL,
    PRIMARY KEY (day, ip_hash)
  )`,
  // One row per group: a server-assigned write counter pushItems bumps on every push, which
  // pullItems pages by for clients that send `since_seq` - see pushItems for why this exists.
  `CREATE TABLE IF NOT EXISTS sync_seq (
    sync_id TEXT PRIMARY KEY,
    value INTEGER NOT NULL DEFAULT 0
  )`,
]

/**
 * Runs once per isolate. Every `SCHEMA` statement is idempotently safe to repeat, but
 * `sync_blobs.seq` was added after those tables existed in production, so it needs its own
 * guarded `ALTER TABLE` plus a one-time backfill: rows written before this column existed get
 * `seq = 0` from the column default, which a from-scratch client pull (`since_seq=0`, i.e.
 * `seq > 0`) would otherwise skip entirely. Backfilling them to 1 - the same value the first
 * real push after migration also produces - puts them on equal, visible footing; ties are broken
 * by `item_id` same as any other pull page.
 */
async function ensureSchema(env: Env): Promise<void> {
  await env.DB.batch(SCHEMA.map((sql) => env.DB.prepare(sql)))
  const columns = (await env.DB.prepare(`SELECT name FROM pragma_table_info('sync_blobs')`).all<{ name: string }>()).results ?? []
  if (!columns.some((column) => column.name === 'seq')) {
    await env.DB.prepare('ALTER TABLE sync_blobs ADD COLUMN seq INTEGER NOT NULL DEFAULT 0').run()
    await env.DB.prepare('UPDATE sync_blobs SET seq = 1 WHERE seq = 0').run()
  }
  await env.DB.prepare('CREATE INDEX IF NOT EXISTS sync_blobs_pull_seq ON sync_blobs (sync_id, seq, item_id)').run()
}

let schemaReady: Promise<unknown> | undefined

async function route(request: Request, env: Env): Promise<Response> {
  // Only the desktop app talks to this server. A browser always sends Origin, so a web page cannot
  // use a visitor's browser to reach it.
  if (request.headers.has('Origin')) return json({ error: 'forbidden' }, 403)

  const { pathname } = new URL(request.url)
  const method = request.method
  if (pathname === '/v1/groups') {
    if (method === 'POST') return createGroup(request, env)
    if (method === 'DELETE') return deleteGroup(request, env)
    return json({ error: 'method_not_allowed' }, 405)
  }
  if (pathname === '/v1/items') {
    if (method === 'GET') return pullItems(request, env)
    if (method === 'POST') return pushItems(request, env)
    return json({ error: 'method_not_allowed' }, 405)
  }
  return json({ error: 'not_found' }, 404)
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    try {
      schemaReady ??= ensureSchema(env)
      await schemaReady
    } catch (error) {
      schemaReady = undefined // retry on the next request instead of caching the failure
      throw error
    }
    return route(request, env)
  },
}

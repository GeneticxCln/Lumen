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
    env.DB.prepare('DELETE FROM sync_groups WHERE last_seen_at < ?').bind(cutoff),
    env.DB.prepare('DELETE FROM group_creations WHERE day < ?').bind(day - 2),
    env.DB.prepare('INSERT INTO sync_groups (sync_id, verifier, created_at, last_seen_at) VALUES (?, ?, ?, ?)').bind(
      syncId,
      verifier,
      now,
      now,
    ),
  ])
  return json({ created: true }, 201)
}

async function deleteGroup(request: Request, env: Env): Promise<Response> {
  const auth = await authenticate(request, env)
  if (auth instanceof Response) return auth
  await env.DB.batch([
    env.DB.prepare('DELETE FROM sync_blobs WHERE sync_id = ?').bind(auth.syncId),
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
  if ((held?.n ?? 0) + items.length > intVar(env.MAX_ITEMS, 20_000)) return json({ error: 'quota_exceeded' }, 413)

  // Last write wins: a stale push never replaces a newer record.
  await env.DB.batch(
    items.map((item) =>
      env.DB.prepare(
        `INSERT INTO sync_blobs (sync_id, item_id, updated_at, deleted, blob) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (sync_id, item_id) DO UPDATE SET
           updated_at = excluded.updated_at,
           deleted = excluded.deleted,
           blob = excluded.blob
         WHERE excluded.updated_at > sync_blobs.updated_at`,
      ).bind(auth.syncId, item.id, item.updated_at, item.deleted ? 1 : 0, item.blob),
    ),
  )
  return json({ ok: true, received: items.length })
}

async function pullItems(request: Request, env: Env): Promise<Response> {
  const auth = await authenticate(request, env)
  if (auth instanceof Response) return auth
  const params = new URL(request.url).searchParams
  const since = Number(params.get('since') ?? '0')
  if (!Number.isSafeInteger(since) || since < 0) return json({ error: 'invalid_request' }, 400)
  const afterId = params.get('after_id')
  if (afterId !== null && !ITEM_ID_RE.test(afterId)) return json({ error: 'invalid_request' }, 400)

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
  `CREATE INDEX IF NOT EXISTS sync_blobs_pull ON sync_blobs (sync_id, updated_at, item_id)`,
  `CREATE TABLE IF NOT EXISTS group_creations (
    day INTEGER NOT NULL,
    ip_hash TEXT NOT NULL,
    n INTEGER NOT NULL,
    PRIMARY KEY (day, ip_hash)
  )`,
]

// Applied once per isolate; every statement is idempotent.
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
      schemaReady ??= env.DB.batch(SCHEMA.map((sql) => env.DB.prepare(sql)))
      await schemaReady
    } catch (error) {
      schemaReady = undefined // retry on the next request instead of caching the failure
      throw error
    }
    return route(request, env)
  },
}

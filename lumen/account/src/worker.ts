// Account Worker for Lumen. Browser sign-in is handled by Freebuff (cookie
// session). The desktop app cannot use that cookie, so it signs in through the
// system browser: the Worker issues a one-time code to a loopback redirect, and
// the app exchanges that code (with its PKCE verifier) for its own app token.
import { createFreebuffAuth, isSameOriginRequest } from '@freebuff/auth'

import { applySchema } from './schema'

import type { FreebuffAuth } from '@freebuff/auth'

export interface Env {
  DB: D1Database
  FREEBUFF_CLIENT_ID: string
  FREEBUFF_ISSUER?: string
  APP_ORIGIN: string
  /** Set to "1" for local development only. */
  ALLOW_INSECURE_LOCALHOST?: string
}

type Handler = (request: Request, env: Env, auth: FreebuffAuth) => Promise<Response>

const CODE_TTL_MS = 60_000
const DAY_MS = 24 * 60 * 60 * 1000
// A token lasts two weeks and renews itself while the app keeps syncing, so a device that stops
// being used drops out of the account on its own instead of holding a month-long credential.
const TOKEN_TTL_MS = 14 * DAY_MS
const TOKEN_RENEW_WITHIN_MS = 7 * DAY_MS
/** Rows one account may hold. Far above real use; stops an account from filling the database. */
const SYNC_MAX_ITEMS_PER_ACCOUNT = 20_000
/** Deletion markers older than this are no longer needed by any device that syncs. */
const TOMBSTONE_KEEP_MS = 90 * DAY_MS
const NO_STORE = { 'Cache-Control': 'private, no-store' }

const STATE_RE = /^[A-Za-z0-9_-]{16,128}$/
const CHALLENGE_RE = /^[A-Za-z0-9_-]{43}$/
const VERIFIER_RE = /^[A-Za-z0-9._~-]{43,128}$/
const BEARER_RE = /^Bearer ([A-Za-z0-9_-]{43})$/

function json(body: unknown, status = 200): Response {
  return Response.json(body, { status, headers: NO_STORE })
}

function base64url(bytes: Uint8Array): string {
  let binary = ''
  for (const byte of bytes) binary += String.fromCharCode(byte)
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

function randomToken(byteLength: number): string {
  return base64url(crypto.getRandomValues(new Uint8Array(byteLength)))
}

async function sha256Base64url(input: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(input))
  return base64url(new Uint8Array(digest))
}

/** Loopback callbacks only: http://127.0.0.1:<port>/callback or http://localhost:<port>/callback. */
function isLoopbackCallback(value: string): boolean {
  let url: URL
  try {
    url = new URL(value)
  } catch {
    return false
  }
  const port = Number(url.port)
  return (
    url.protocol === 'http:' &&
    (url.hostname === '127.0.0.1' || url.hostname === 'localhost') &&
    url.port !== '' &&
    port >= 1024 &&
    port <= 65535 &&
    url.pathname === '/callback' &&
    url.search === '' &&
    url.hash === '' &&
    url.username === '' &&
    url.password === ''
  )
}

function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c] as string)
}

interface AuthorizeRequest {
  redirectUri: string
  state: string
  challenge: string
}

/** Reads and validates the authorize parameters, or null if any is malformed. */
function parseAuthorize(params: URLSearchParams | FormData): AuthorizeRequest | null {
  const redirectUri = String(params.get('redirect_uri') ?? '')
  const state = String(params.get('state') ?? '')
  const challenge = String(params.get('code_challenge') ?? '')
  if (String(params.get('code_challenge_method') ?? '') !== 'S256') return null
  if (!isLoopbackCallback(redirectUri) || !STATE_RE.test(state) || !CHALLENGE_RE.test(challenge)) return null
  return { redirectUri, state, challenge }
}

/**
 * The page that asks the signed-in person to approve the app. Without it, any website could
 * navigate a signed-in browser to /desktop/authorize and hand a code to whatever listens on a
 * local port, which on a shared machine may be another user's process.
 */
function consentPage(request: AuthorizeRequest): string {
  const port = new URL(request.redirectUri).port
  const field = (name: string, value: string) => `<input type="hidden" name="${name}" value="${escapeHtml(value)}">`
  return `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Allow Lumen</title>
<style>body{font:16px system-ui,sans-serif;max-width:30rem;margin:12vh auto;padding:0 1rem;color:#1e2531}button{font:inherit;padding:.6rem 1.2rem;margin-right:.5rem;cursor:pointer}</style></head><body>
<h1>Allow Lumen on this computer?</h1>
<p>A Lumen app on this computer (listening on port <strong>${escapeHtml(port)}</strong>) is asking to sync your profiles, favorites and watch progress with your account.</p>
<p>Only continue if you just chose <em>Sign in</em> in Lumen.</p>
<form method="post" action="/desktop/authorize">${field('redirect_uri', request.redirectUri)}${field('state', request.state)}${field('code_challenge', request.challenge)}${field('code_challenge_method', 'S256')}
<button type="submit">Allow</button><a href="about:blank">Cancel</a></form></body></html>`
}

function htmlResponse(body: string): Response {
  return new Response(body, {
    status: 200,
    headers: {
      'Content-Type': 'text/html; charset=utf-8',
      ...NO_STORE,
      // The approval page must not be framed, and its form may only post back here; the redirect
      // that follows goes to a loopback address, which form-action also governs.
      'Content-Security-Policy':
        "default-src 'none'; style-src 'unsafe-inline'; form-action 'self' http://127.0.0.1:* http://localhost:*; frame-ancestors 'none'; base-uri 'none'",
      'X-Frame-Options': 'DENY',
    },
  })
}

/** Stores a one-time code for the signed-in user and sends the browser back to the app. */
async function issueCode(env: Env, sub: string, request: AuthorizeRequest): Promise<Response> {
  const code = randomToken(32)
  const now = Date.now()
  await env.DB.batch([
    env.DB.prepare('DELETE FROM desktop_codes WHERE expires_at < ?').bind(now),
    env.DB.prepare(
      'INSERT INTO desktop_codes (code_hash, sub, redirect_uri, code_challenge, expires_at) VALUES (?, ?, ?, ?, ?)',
    ).bind(await sha256Base64url(code), sub, request.redirectUri, request.challenge, now + CODE_TTL_MS),
  ])
  const back = new URL(request.redirectUri)
  back.searchParams.set('code', code)
  back.searchParams.set('state', request.state)
  return new Response(null, { status: 302, headers: { Location: back.toString(), ...NO_STORE } })
}

/**
 * The desktop client starts here in the system browser. GET shows an approval page; only a
 * same-origin POST from that page issues the code.
 */
async function desktopAuthorize(request: Request, env: Env, auth: FreebuffAuth): Promise<Response> {
  if (request.method === 'POST') {
    // Browsers always send Origin on a form POST. A missing or foreign one means the form was
    // not ours, so refuse before reading anything.
    if (!request.headers.has('Origin') || !isSameOriginRequest(request, env.APP_ORIGIN)) {
      return json({ error: 'forbidden' }, 403)
    }
    let form: FormData
    try {
      form = await request.formData()
    } catch {
      return json({ error: 'invalid_request' }, 400)
    }
    const parsed = parseAuthorize(form)
    if (!parsed) return json({ error: 'invalid_request' }, 400)
    const user = await auth.getUser(request)
    if (!user) return json({ error: 'Sign in required' }, 401)
    return issueCode(env, user.sub, parsed)
  }

  const url = new URL(request.url)
  const parsed = parseAuthorize(url.searchParams)
  if (!parsed) return json({ error: 'invalid_request' }, 400)

  const user = await auth.getUser(request)
  if (!user) {
    // Send the browser through sign-in, then back to this exact request.
    const signIn = new URL('/auth/freebuff/signin', env.APP_ORIGIN)
    signIn.searchParams.set('returnTo', url.pathname + url.search)
    return new Response(null, { status: 302, headers: { Location: signIn.toString(), ...NO_STORE } })
  }
  return htmlResponse(consentPage(parsed))
}

/** The desktop client exchanges its one-time code for an app token. */
async function desktopToken(request: Request, env: Env): Promise<Response> {
  if (request.method !== 'POST') return json({ error: 'method_not_allowed' }, 405)
  // Browsers always send Origin on cross-site POSTs; the desktop client does not.
  if (request.headers.has('Origin')) return json({ error: 'forbidden' }, 403)

  let body: { code?: unknown; code_verifier?: unknown; redirect_uri?: unknown }
  try {
    body = await request.json()
  } catch {
    return json({ error: 'invalid_request' }, 400)
  }
  const { code, code_verifier: verifier, redirect_uri: redirectUri } = body
  if (
    typeof code !== 'string' ||
    typeof verifier !== 'string' ||
    typeof redirectUri !== 'string' ||
    !VERIFIER_RE.test(verifier) ||
    !isLoopbackCallback(redirectUri)
  ) {
    return json({ error: 'invalid_request' }, 400)
  }

  const codeHash = await sha256Base64url(code)
  const row = await env.DB.prepare(
    'SELECT sub, redirect_uri, code_challenge, expires_at, used FROM desktop_codes WHERE code_hash = ?',
  )
    .bind(codeHash)
    .first<{ sub: string; redirect_uri: string; code_challenge: string; expires_at: number; used: number }>()

  if (!row || row.used !== 0 || row.expires_at < Date.now() || row.redirect_uri !== redirectUri) {
    return json({ error: 'invalid_grant' }, 400)
  }
  if ((await sha256Base64url(verifier)) !== row.code_challenge) {
    return json({ error: 'invalid_grant' }, 400)
  }

  // Consume the code atomically so it works exactly once.
  const consumed = await env.DB.prepare('UPDATE desktop_codes SET used = 1 WHERE code_hash = ? AND used = 0')
    .bind(codeHash)
    .run()
  if (consumed.meta.changes !== 1) return json({ error: 'invalid_grant' }, 400)

  const token = randomToken(32)
  const expiresAt = Date.now() + TOKEN_TTL_MS
  // Expired and revoked sessions are dead weight; clear them while we are writing anyway.
  await env.DB.prepare('DELETE FROM app_sessions WHERE expires_at < ? OR (revoked = 1 AND created_at < ?)')
    .bind(Date.now(), Date.now() - 30 * DAY_MS)
    .run()
  await env.DB.prepare(
    'INSERT INTO app_sessions (token_hash, sub, created_at, expires_at) VALUES (?, ?, ?, ?)',
  )
    .bind(await sha256Base64url(token), row.sub, Date.now(), expiresAt)
    .run()

  return json({ access_token: token, token_type: 'Bearer', expires_at: expiresAt })
}

/** Resolves the Bearer app token to a user sub, or null. */
async function sessionSub(request: Request, env: Env): Promise<string | null> {
  const match = BEARER_RE.exec(request.headers.get('Authorization') ?? '')
  if (!match) return null
  const row = await env.DB.prepare('SELECT sub, expires_at, revoked FROM app_sessions WHERE token_hash = ?')
    .bind(await sha256Base64url(match[1]))
    .first<{ sub: string; expires_at: number; revoked: number }>()
  const now = Date.now()
  if (!row || row.revoked !== 0 || row.expires_at < now) return null
  if (row.expires_at - now < TOKEN_RENEW_WITHIN_MS) {
    await env.DB.prepare('UPDATE app_sessions SET expires_at = ? WHERE token_hash = ?')
      .bind(now + TOKEN_TTL_MS, await sha256Base64url(match[1]))
      .run()
  }
  return row.sub
}

async function desktopMe(request: Request, env: Env): Promise<Response> {
  const sub = await sessionSub(request, env)
  if (!sub) return json({ error: 'Sign in required' }, 401)
  return json({ sub })
}

async function desktopSignOut(request: Request, env: Env): Promise<Response> {
  if (request.method !== 'POST') return json({ error: 'method_not_allowed' }, 405)
  const match = BEARER_RE.exec(request.headers.get('Authorization') ?? '')
  if (!match) return json({ error: 'Sign in required' }, 401)
  await env.DB.prepare('UPDATE app_sessions SET revoked = 1 WHERE token_hash = ?')
    .bind(await sha256Base64url(match[1]))
    .run()
  return json({ ok: true })
}

const SYNC_KINDS = new Set(['profile', 'favorite', 'progress'])
const SYNC_MAX_PUSH = 500
const SYNC_MAX_PULL = 500
const SYNC_MAX_PAYLOAD_BYTES = 4096
const SYNC_MAX_KEY_LENGTH = 512

interface SyncPush {
  kind: string
  key: string
  payload: unknown
  updated_at: number
  deleted: boolean
}

function parseSyncPush(value: unknown): SyncPush[] | null {
  if (!value || typeof value !== 'object' || !Array.isArray((value as { items?: unknown }).items)) return null
  const items = (value as { items: unknown[] }).items
  if (items.length > SYNC_MAX_PUSH) return null
  const parsed: SyncPush[] = []
  for (const item of items) {
    if (!item || typeof item !== 'object') return null
    const { kind, key, payload, updated_at: updatedAt, deleted } = item as Record<string, unknown>
    if (
      typeof kind !== 'string' ||
      !SYNC_KINDS.has(kind) ||
      typeof key !== 'string' ||
      key.length === 0 ||
      key.length > SYNC_MAX_KEY_LENGTH ||
      typeof updatedAt !== 'number' ||
      !Number.isSafeInteger(updatedAt) ||
      updatedAt < 0 ||
      typeof deleted !== 'boolean' ||
      payload === undefined
    ) {
      return null
    }
    const serialized = JSON.stringify(payload)
    if (new TextEncoder().encode(serialized).length > SYNC_MAX_PAYLOAD_BYTES) return null
    parsed.push({ kind, key, payload, updated_at: updatedAt, deleted })
  }
  return parsed
}

/**
 * GET: pull records changed after `since` (ms). POST: push local records.
 * Each row is keyed by the signed-in sub, so a token only ever touches its own data.
 */
async function desktopSync(request: Request, env: Env): Promise<Response> {
  // The desktop client sends no Origin. Refuse browser requests outright.
  if (request.headers.has('Origin')) return json({ error: 'forbidden' }, 403)
  const sub = await sessionSub(request, env)
  if (!sub) return json({ error: 'Sign in required' }, 401)

  if (request.method === 'GET') {
    const params = new URL(request.url).searchParams
    const since = Number(params.get('since') ?? '0')
    if (!Number.isSafeInteger(since) || since < 0) return json({ error: 'invalid_request' }, 400)

    // Rows are paged in (updated_at, kind, item_key) order. A page boundary can fall in the middle
    // of rows that share one updated_at (a bulk migration stamps them all alike), so the next
    // request names the last row it received and the server resumes strictly after it. Without
    // that, `updated_at > since` would skip every remaining row with the boundary's timestamp.
    const afterKind = params.get('after_kind')
    const afterKey = params.get('after_key')
    if ((afterKind === null) !== (afterKey === null)) return json({ error: 'invalid_request' }, 400)
    if (afterKey !== null && afterKey.length > SYNC_MAX_KEY_LENGTH) return json({ error: 'invalid_request' }, 400)

    const statement =
      afterKind !== null && afterKey !== null
        ? env.DB.prepare(
            `SELECT kind, item_key, payload, updated_at, deleted FROM sync_items
             WHERE sub = ? AND (updated_at > ? OR (updated_at = ? AND (kind > ? OR (kind = ? AND item_key > ?))))
             ORDER BY updated_at, kind, item_key LIMIT ?`,
          ).bind(sub, since, since, afterKind, afterKind, afterKey, SYNC_MAX_PULL + 1)
        : env.DB.prepare(
            'SELECT kind, item_key, payload, updated_at, deleted FROM sync_items WHERE sub = ? AND updated_at > ? ORDER BY updated_at, kind, item_key LIMIT ?',
          ).bind(sub, since, SYNC_MAX_PULL + 1)
    const rows = await statement.all<{ kind: string; item_key: string; payload: string; updated_at: number; deleted: number }>()
    const found = rows.results ?? []
    const page = found.slice(0, SYNC_MAX_PULL)
    return json({
      items: page.map((row) => ({
        kind: row.kind,
        key: row.item_key,
        payload: JSON.parse(row.payload) as unknown,
        updated_at: row.updated_at,
        deleted: row.deleted === 1,
      })),
      has_more: found.length > SYNC_MAX_PULL,
    })
  }

  if (request.method === 'POST') {
    let body: unknown
    try {
      body = await request.json()
    } catch {
      return json({ error: 'invalid_request' }, 400)
    }
    const items = parseSyncPush(body)
    if (!items) return json({ error: 'invalid_request' }, 400)
    if (items.length === 0) return json({ ok: true, received: 0 })

    // Reclaim old deletion markers first, so the quota never traps an account that only has
    // tombstones left. Then refuse growth past the cap.
    await env.DB.prepare('DELETE FROM sync_items WHERE sub = ? AND deleted = 1 AND updated_at < ?')
      .bind(sub, Date.now() - TOMBSTONE_KEEP_MS)
      .run()
    const held = await env.DB.prepare('SELECT COUNT(*) AS n FROM sync_items WHERE sub = ?')
      .bind(sub)
      .first<{ n: number }>()
    if ((held?.n ?? 0) + items.length > SYNC_MAX_ITEMS_PER_ACCOUNT) {
      return json({ error: 'quota_exceeded' }, 413)
    }

    // Last write wins: a stale push never overwrites a newer row.
    const statements = items.map((item) =>
      env.DB.prepare(
        `INSERT INTO sync_items (sub, kind, item_key, payload, updated_at, deleted) VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (sub, kind, item_key) DO UPDATE SET
           payload = excluded.payload,
           updated_at = excluded.updated_at,
           deleted = excluded.deleted
         WHERE excluded.updated_at > sync_items.updated_at`,
      ).bind(
        sub,
        item.kind,
        item.key,
        JSON.stringify(item.payload),
        item.updated_at,
        item.deleted ? 1 : 0,
      ),
    )
    await env.DB.batch(statements)
    return json({ ok: true, received: items.length })
  }

  return json({ error: 'method_not_allowed' }, 405)
}

const routes: Record<string, Handler> = {
  '/api/desktop/sync': (request, env) => desktopSync(request, env),
  '/desktop/authorize': (request, env, auth) => {
    if (request.method !== 'GET' && request.method !== 'POST') {
      return Promise.resolve(json({ error: 'method_not_allowed' }, 405))
    }
    return desktopAuthorize(request, env, auth)
  },
  '/api/desktop/token': (request, env) => desktopToken(request, env),
  '/api/desktop/me': (request, env) => desktopMe(request, env),
  '/api/desktop/signout': (request, env) => {
    // Sign-out from a browser is a state change; require same origin as well.
    if (request.headers.has('Origin') && !isSameOriginRequest(request, env.APP_ORIGIN)) {
      return Promise.resolve(json({ error: 'forbidden' }, 403))
    }
    return desktopSignOut(request, env)
  },
}

let auth: FreebuffAuth | undefined
let app: ((request: Request, env: Env) => Promise<Response>) | undefined
let schemaReady: Promise<unknown> | undefined

function setup(env: Env) {
  // One instance per isolate caches discovery and signing keys.
  auth ??= createFreebuffAuth({
    clientId: env.FREEBUFF_CLIENT_ID,
    issuer: env.FREEBUFF_ISSUER,
    baseUrl: env.APP_ORIGIN,
    allowInsecureLocalhost: env.ALLOW_INSECURE_LOCALHOST === '1',
  })
  const signedIn = auth
  app ??= signedIn.wrap(async (request: Request, env: Env) => {
    try {
      schemaReady ??= applySchema(env.DB)
      await schemaReady
    } catch (error) {
      // Allow a retry on the next request instead of caching the failure.
      schemaReady = undefined
      throw error
    }
    const route = routes[new URL(request.url).pathname]
    if (!route) return json({ error: 'not_found' }, 404)
    return route(request, env, signedIn)
  })
  return app
}

export default {
  fetch(request: Request, env: Env): Promise<Response> {
    return setup(env)(request, env)
  },
}

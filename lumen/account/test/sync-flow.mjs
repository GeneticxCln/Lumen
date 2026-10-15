// End-to-end check of /api/desktop/sync against `npm run dev`.
// Two test users get app tokens the same way the desktop client does: a
// one-time code is seeded into local D1 (the row desktopAuthorize writes),
// then exchanged at /api/desktop/token.
import { createHash, randomBytes } from 'node:crypto'
import { execFileSync } from 'node:child_process'

const BASE = process.env.ACCOUNT_BASE ?? 'http://127.0.0.1:8787'
const REDIRECT = 'http://127.0.0.1:53111/callback'

const b64url = (buf) => buf.toString('base64url')
const sha256 = (s) => b64url(createHash('sha256').update(s).digest())
const rand = () => b64url(randomBytes(6))

let failures = 0
function check(name, ok, detail = '') {
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ` (${detail})` : ''}`)
  if (!ok) failures++
}

function seedCode({ sub, code, challenge, expiresAt }) {
  const sql = `INSERT INTO desktop_codes (code_hash, sub, redirect_uri, code_challenge, expires_at) VALUES ('${sha256(code)}', '${sub}', '${REDIRECT}', '${challenge}', ${expiresAt});`
  execFileSync('npx', ['wrangler', 'd1', 'execute', 'DB', '--local', '--command', sql], {
    cwd: new URL('..', import.meta.url),
    stdio: 'pipe',
  })
}

async function signIn(sub) {
  const verifier = b64url(randomBytes(32))
  const code = b64url(randomBytes(32))
  seedCode({ sub, code, challenge: sha256(verifier), expiresAt: Date.now() + 60_000 })
  const res = await fetch(`${BASE}/api/desktop/token`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ code, code_verifier: verifier, redirect_uri: REDIRECT }),
  })
  const body = await res.json()
  if (res.status !== 200) throw new Error(`token exchange failed for ${sub}: ${res.status}`)
  return body.access_token
}

async function sync(token, { method = 'GET', query = '', body, origin } = {}) {
  const headers = {}
  if (token) headers.Authorization = `Bearer ${token}`
  if (origin) headers.Origin = origin
  if (body !== undefined) headers['Content-Type'] = 'application/json'
  const res = await fetch(`${BASE}/api/desktop/sync${query}`, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  const text = await res.text()
  let json = null
  try {
    json = JSON.parse(text)
  } catch {
    // Non-JSON responses are checked by status only.
  }
  return { status: res.status, json }
}

const push = (token, items) => sync(token, { method: 'POST', body: { items } })
const pull = (token, since = 0, after) =>
  sync(token, {
    query: `?since=${since}${after ? `&after_kind=${encodeURIComponent(after.kind)}&after_key=${encodeURIComponent(after.key)}` : ''}`,
  })

const suffix = rand()
const subA = `sync-a-${suffix}`
const subB = `sync-b-${suffix}`
const tokenA = await signIn(subA)
const tokenB = await signIn(subB)

// 1. Auth required.
check('pull without token is 401', (await sync(null)).status === 401)
check('push without token is 401', (await sync(null, { method: 'POST', body: { items: [] } })).status === 401)
check('bad token is 401', (await sync('AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA')).status === 401)

// 2. Browser requests are refused even with a valid token.
const browser = await sync(tokenA, { origin: 'https://evil.example' })
check('pull with browser Origin is 403', browser.status === 403, String(browser.status))

// 3. Push and pull back for user A.
const first = await push(tokenA, [
  { kind: 'profile', key: 'p1', payload: { name: 'Kids', pin_hash: null }, updated_at: 1000, deleted: false },
  { kind: 'favorite', key: 'http://x.example/a.m3u8', payload: { title: 'A' }, updated_at: 1000, deleted: false },
  { kind: 'progress', key: 'http://x.example/a.m3u8', payload: { seconds: 42 }, updated_at: 1000, deleted: false },
])
check('push returns ok', first.status === 200 && first.json?.ok === true, String(first.status))

const pullA = await pull(tokenA)
check('pull returns the three pushed items', pullA.status === 200 && pullA.json.items.length === 3, String(pullA.json?.items?.length))
check(
  'pulled payloads match what was pushed',
  pullA.json?.items?.find((i) => i.kind === 'progress')?.payload?.seconds === 42,
)

// 4. Users are isolated: B sees nothing from A, and the same key is a separate record for B.
const pullB0 = await pull(tokenB)
check('user B sees no records from user A', pullB0.status === 200 && pullB0.json.items.length === 0, String(pullB0.json?.items?.length))
await push(tokenB, [{ kind: 'profile', key: 'p1', payload: { name: 'Mine' }, updated_at: 2000, deleted: false }])
const pullA1 = await pull(tokenA)
check(
  'user B writing the same key does not change user A',
  pullA1.json.items.find((i) => i.kind === 'profile' && i.key === 'p1')?.payload?.name === 'Kids',
)

// 5. Last write wins: a stale push is ignored, a newer one replaces the row.
await push(tokenA, [{ kind: 'profile', key: 'p1', payload: { name: 'Stale' }, updated_at: 500, deleted: false }])
const pullA2 = await pull(tokenA)
check(
  'stale push does not overwrite a newer row',
  pullA2.json.items.find((i) => i.kind === 'profile' && i.key === 'p1')?.payload?.name === 'Kids',
)
await push(tokenA, [{ kind: 'profile', key: 'p1', payload: { name: 'Renamed' }, updated_at: 3000, deleted: false }])
const pullA3 = await pull(tokenA)
check(
  'newer push replaces the row',
  pullA3.json.items.find((i) => i.kind === 'profile' && i.key === 'p1')?.payload?.name === 'Renamed',
)

// 6. Tombstones: a deletion syncs as a deleted row.
await push(tokenA, [{ kind: 'favorite', key: 'http://x.example/a.m3u8', payload: {}, updated_at: 4000, deleted: true }])
const pullA4 = await pull(tokenA, 3500)
const tomb = pullA4.json.items.find((i) => i.kind === 'favorite')
check('deletion syncs as a tombstone', tomb?.deleted === true && tomb?.updated_at === 4000, JSON.stringify(tomb))

// 7. Cursor: only rows changed after `since` come back.
const after = await pull(tokenA, 1000)
check(
  'pull with since returns only newer rows',
  after.json.items.every((i) => i.updated_at > 1000) && after.json.items.length === 2,
  String(after.json.items.length),
)

// 7b. Paging across rows that share one timestamp. A bulk migration stamps many records alike;
// the page boundary must not drop the rest of them.
const bulk = await signIn(`sync-bulk-${suffix}`)
const SAME = 7_000_000
for (let start = 0; start < 1200; start += 500) {
  const items = Array.from({ length: Math.min(500, 1200 - start) }, (_, n) => ({
    kind: 'favorite',
    key: `http://bulk.example/${String(start + n).padStart(5, '0')}.m3u8`,
    payload: { n: start + n },
    updated_at: SAME,
    deleted: false,
  }))
  await push(bulk, items)
}
const seen = new Set()
let cursor = { since: 0, after: undefined }
let pages = 0
for (;;) {
  const page = await pull(bulk, cursor.since, cursor.after)
  if (page.status !== 200) break
  pages++
  for (const item of page.json.items) seen.add(item.key)
  if (!page.json.has_more || page.json.items.length === 0) break
  const last = page.json.items[page.json.items.length - 1]
  cursor = { since: last.updated_at, after: { kind: last.kind, key: last.key } }
  if (pages > 10) break
}
check('1200 rows sharing one timestamp all arrive across pages', seen.size === 1200, `${seen.size} in ${pages} pages`)
const oneSided = await sync(bulk, { query: '?since=0&after_kind=favorite' })
check('rejects after_kind without after_key', oneSided.status === 400, String(oneSided.status))

// 8. Input validation.
const bad = [
  ['unknown kind', [{ kind: 'playlist', key: 'k', payload: {}, updated_at: 1, deleted: false }]],
  ['empty key', [{ kind: 'profile', key: '', payload: {}, updated_at: 1, deleted: false }]],
  ['negative updated_at', [{ kind: 'profile', key: 'k', payload: {}, updated_at: -1, deleted: false }]],
  ['non-boolean deleted', [{ kind: 'profile', key: 'k', payload: {}, updated_at: 1, deleted: 'no' }]],
  ['oversized payload', [{ kind: 'profile', key: 'k', payload: { x: 'a'.repeat(5000) }, updated_at: 1, deleted: false }]],
  ['too many items', Array.from({ length: 501 }, (_, n) => ({ kind: 'profile', key: `k${n}`, payload: {}, updated_at: 1, deleted: false }))],
]
for (const [name, items] of bad) {
  const res = await push(tokenA, items)
  check(`rejects ${name}`, res.status === 400, String(res.status))
}
const badSince = await pull(tokenA, -5)
check('rejects a negative since', badSince.status === 400, String(badSince.status))

// 9. Sign-out revokes the token for sync too.
const signOut = await fetch(`${BASE}/api/desktop/signout`, {
  method: 'POST',
  headers: { Authorization: `Bearer ${tokenA}` },
})
check('sign-out succeeds', signOut.status === 200, String(signOut.status))
check('sync is refused after sign-out', (await pull(tokenA)).status === 401)
check('user B still syncs after user A signs out', (await pull(tokenB)).status === 200)

console.log(failures === 0 ? '\nAll sync checks passed.' : `\n${failures} sync check(s) failed.`)
process.exitCode = failures === 0 ? 0 : 1

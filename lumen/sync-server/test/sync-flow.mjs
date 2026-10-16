// End-to-end check of the sync server against `npm run dev` (which sets POW_BITS=8,
// GROUP_CREATE_LIMIT=4 and MAX_ITEMS=1500 so each limit can be exercised quickly).
//
// There is no login to fake: a client holds a random secret, registers a group with SHA-256 of
// its token, and sends opaque blobs. The server never sees anything it could decrypt, so these
// tests use random strings for blobs and check only what the server is responsible for.
import { createHash, randomBytes } from 'node:crypto'

const BASE = process.env.SYNC_BASE ?? 'http://127.0.0.1:8787'
const POW_BITS = Number(process.env.TEST_POW_BITS ?? '8')
const CREATE_LIMIT = Number(process.env.TEST_CREATE_LIMIT ?? '4')
const MAX_ITEMS = Number(process.env.TEST_MAX_ITEMS ?? '1500')

const sha = (text) => createHash('sha256').update(text).digest()
const hex = (buf) => buf.toString('hex')
const b64url = (buf) => buf.toString('base64url')
// A distinct address per client, so the per-address creation cap only applies where a test asks for it.
const randomIp = () => `10.${randomBytes(1)[0]}.${randomBytes(1)[0]}.${randomBytes(1)[0]}`

let failures = 0
function check(name, ok, detail = '') {
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ` (${detail})` : ''}`)
  if (!ok) failures++
}

function leadingZeroBits(buf) {
  let bits = 0
  for (const byte of buf) {
    if (byte === 0) { bits += 8; continue }
    return bits + Math.clz32(byte) - 24
  }
  return bits
}
const powValid = (syncId, nonce, bits) => leadingZeroBits(sha(`${syncId}:${nonce}`)) >= bits
function solve(syncId, bits = POW_BITS) {
  for (let nonce = 0; ; nonce++) if (powValid(syncId, nonce, bits)) return nonce
}
function failingNonce(syncId, bits = POW_BITS) {
  for (let nonce = 0; ; nonce++) if (!powValid(syncId, nonce, bits)) return nonce
}

async function call(method, path, { headers = {}, body, query = '' } = {}) {
  const res = await fetch(`${BASE}${path}${query}`, {
    method,
    headers: { ...(body === undefined ? {} : { 'Content-Type': 'application/json' }), ...headers },
    body: body === undefined ? undefined : typeof body === 'string' ? body : JSON.stringify(body),
  })
  const text = await res.text()
  let json = null
  try { json = JSON.parse(text) } catch { /* status-only responses */ }
  return { status: res.status, json }
}

// A client's view of one group: its id, token and the headers every call carries.
function newGroup(ip = randomIp()) {
  const syncId = hex(randomBytes(16))
  const token = b64url(randomBytes(32)) // 43 characters, like the real auth token
  return {
    syncId, token, ip,
    verifier: hex(sha(token)),
    headers: { 'X-Sync-Id': syncId, Authorization: `Bearer ${token}`, 'cf-connecting-ip': ip },
  }
}
const register = (g, nonce = solve(g.syncId)) =>
  call('POST', '/v1/groups', { headers: { 'cf-connecting-ip': g.ip }, body: { sync_id: g.syncId, verifier: g.verifier, pow_nonce: nonce } })
const push = (g, items) => call('POST', '/v1/items', { headers: g.headers, body: { items } })
const pull = (g, since = 0, afterId) =>
  call('GET', '/v1/items', { headers: g.headers, query: `?since=${since}${afterId ? `&after_id=${afterId}` : ''}` })
const itemId = (n) => n.toString(16).padStart(32, '0')
const blob = () => b64url(randomBytes(48))
const rec = (n, updated_at, deleted = false) => ({ id: itemId(n), updated_at, deleted, blob: blob() })

// 1. A browser can never use a visitor's session to reach the server.
{
  const g = newGroup()
  const res = await call('POST', '/v1/groups', { headers: { Origin: 'https://evil.example' }, body: { sync_id: g.syncId, verifier: g.verifier, pow_nonce: 0 } })
  check('a request carrying an Origin header is refused', res.status === 403, String(res.status))
  const read = await call('GET', '/v1/items', { headers: { ...g.headers, Origin: 'https://evil.example' } })
  check('so is a read', read.status === 403, String(read.status))
}

// 2. Creating a group.
const a = newGroup()
{
  const bad = await call('POST', '/v1/groups', { body: { sync_id: 'nope', verifier: a.verifier, pow_nonce: 0 } })
  check('a malformed group id is rejected', bad.status === 400, String(bad.status))
  const noVerifier = await call('POST', '/v1/groups', { body: { sync_id: a.syncId, verifier: 'short', pow_nonce: 0 } })
  check('a malformed verifier is rejected', noVerifier.status === 400, String(noVerifier.status))
  const weak = await register(a, failingNonce(a.syncId))
  check('a group without proof of work is refused', weak.status === 400 && weak.json?.error === 'invalid_pow', `${weak.status} ${weak.json?.error}`)
  const made = await register(a)
  check('a group with proof of work is created', made.status === 201 && made.json?.created === true, String(made.status))
  const again = await register(a, 0)
  check('registering the same group again is accepted (and needs no new proof)', again.status === 200 && again.json?.created === false, String(again.status))
  const intruder = { ...a, verifier: hex(sha('different token')) }
  const stolen = await register(intruder, 0)
  check('claiming an existing group id with another token is refused', stolen.status === 409, String(stolen.status))
}

// 3. Authentication.
{
  const none = await call('GET', '/v1/items')
  check('no credentials is 401', none.status === 401, String(none.status))
  const wrong = await call('GET', '/v1/items', { headers: { ...a.headers, Authorization: `Bearer ${b64url(randomBytes(32))}` } })
  check('a wrong token is 401', wrong.status === 401, String(wrong.status))
  const stranger = newGroup()
  const unknown = await pull(stranger)
  check('an unknown group is 404, so a valid key can tell nothing is stored yet', unknown.status === 404, String(unknown.status))
  const badId = await call('GET', '/v1/items', { headers: { ...a.headers, 'X-Sync-Id': 'xyz' } })
  check('a malformed group id header is 401', badId.status === 401, String(badId.status))
  const badToken = await call('GET', '/v1/items', { headers: { ...a.headers, Authorization: 'Bearer short' } })
  check('a malformed token is 401', badToken.status === 401, String(badToken.status))
}

// 4. Round trip, isolation, last-write-wins, tombstones.
const first = await push(a, [rec(1, 1000), rec(2, 1000), rec(3, 1000)])
check('a push is accepted', first.status === 200 && first.json?.received === 3, String(first.status))
const pulled = await pull(a)
check('a pull returns what was pushed', pulled.status === 200 && pulled.json.items.length === 3 && pulled.json.has_more === false, String(pulled.json?.items?.length))
check('items come back in (updated_at, id) order with their blobs', pulled.json.items.map((i) => i.id).join() === [1, 2, 3].map(itemId).join() && pulled.json.items.every((i) => typeof i.blob === 'string'))
{
  const b = newGroup()
  await register(b)
  check('another group sees none of it', (await pull(b)).json.items.length === 0)
  await push(b, [rec(1, 9999)])
  const stillA = await pull(a)
  check('another group writing the same id does not change this one', stillA.json.items.find((i) => i.id === itemId(1)).updated_at === 1000)
}
{
  const marker = blob()
  await push(a, [{ id: itemId(1), updated_at: 500, deleted: false, blob: marker }])
  check('a stale push does not replace a newer record', (await pull(a)).json.items.find((i) => i.id === itemId(1)).blob !== marker)
  const fresh = blob()
  await push(a, [{ id: itemId(1), updated_at: 3000, deleted: false, blob: fresh }])
  check('a newer push replaces the record', (await pull(a)).json.items.find((i) => i.id === itemId(1)).blob === fresh)
  await push(a, [{ id: itemId(2), updated_at: 4000, deleted: true, blob: blob() }])
  const tomb = (await pull(a, 3500)).json.items
  check('a deletion comes back as a tombstone', tomb.length === 1 && tomb[0].id === itemId(2) && tomb[0].deleted === true, JSON.stringify(tomb.map((i) => [i.id.slice(-2), i.deleted])))
  const newer = await pull(a, 1000)
  check('since returns only newer records', newer.json.items.every((i) => i.updated_at > 1000) && newer.json.items.length === 2, String(newer.json.items.length))
}

// 5. Paging across records that share one timestamp (what a bulk import produces).
const bulk = newGroup()
await register(bulk)
{
  const SAME = 7_000_000
  for (let start = 0; start < 1200; start += 500) {
    await push(bulk, Array.from({ length: Math.min(500, 1200 - start) }, (_, n) => rec(start + n + 1, SAME)))
  }
  const seen = new Set()
  let cursor = { since: 0, after: undefined }
  let pages = 0
  for (; pages < 10;) {
    const page = await pull(bulk, cursor.since, cursor.after)
    pages++
    page.json.items.forEach((i) => seen.add(i.id))
    if (!page.json.has_more) break
    const last = page.json.items.at(-1)
    cursor = { since: last.updated_at, after: last.id }
  }
  check('1200 records sharing one timestamp all arrive across pages', seen.size === 1200, `${seen.size} in ${pages} pages`)

  // The shipped-before-this-change client advanced by timestamp alone; show why that lost data.
  const legacy = new Set()
  let since = 0
  for (let i = 0; i < 10; i++) {
    const page = await pull(bulk, since)
    page.json.items.forEach((x) => legacy.add(x.id))
    if (!page.json.has_more) break
    since = Math.max(...page.json.items.map((x) => x.updated_at))
  }
  check('(a timestamp-only cursor would have stopped at 500)', legacy.size === 500, String(legacy.size))
}

// 6. Validation.
{
  const cases = [
    ['a malformed item id', [{ id: 'xyz', updated_at: 1, deleted: false, blob: blob() }]],
    ['an uppercase item id', [{ id: itemId(1).toUpperCase().replace(/0/g, 'A'), updated_at: 1, deleted: false, blob: blob() }]],
    ['a negative updated_at', [{ id: itemId(5), updated_at: -1, deleted: false, blob: blob() }]],
    ['a non-boolean deleted', [{ id: itemId(5), updated_at: 1, deleted: 'no', blob: blob() }]],
    ['an empty blob', [{ id: itemId(5), updated_at: 1, deleted: false, blob: '' }]],
    ['a blob with characters outside base64url', [{ id: itemId(5), updated_at: 1, deleted: false, blob: 'not base64 !!' }]],
    ['an oversized blob', [{ id: itemId(5), updated_at: 1, deleted: false, blob: 'A'.repeat(6145) }]],
    ['too many items', Array.from({ length: 501 }, (_, n) => rec(n + 1, 1))],
  ]
  for (const [name, items] of cases) {
    const res = await push(a, items)
    check(`rejects ${name}`, res.status === 400, String(res.status))
  }
  const garbage = await call('POST', '/v1/items', { headers: a.headers, body: 'not json' })
  check('rejects a body that is not JSON', garbage.status === 400, String(garbage.status))
  check('rejects a negative since', (await pull(a, -5)).status === 400)
  check('rejects a malformed after_id', (await pull(a, 0, 'zzz')).status === 400)
}

// 7. Row cap per group (the server runs with MAX_ITEMS=1500; `bulk` holds 1200).
{
  const ok = await push(bulk, Array.from({ length: MAX_ITEMS - 1200 }, (_, n) => rec(5000 + n, 8_000_000)))
  check('a push up to the cap is accepted', ok.status === 200, String(ok.status))
  const over = await push(bulk, [rec(9999, 8_000_001)])
  check('a push over the cap is refused', over.status === 413 && over.json?.error === 'quota_exceeded', String(over.status))
}

// 8. Creating many groups from one address is limited (the server runs with GROUP_CREATE_LIMIT=4).
{
  const ip = randomIp()
  const results = []
  for (let n = 0; n < CREATE_LIMIT + 1; n++) results.push((await register(newGroup(ip))).status)
  check('the daily cap on group creation per address applies', results.slice(0, CREATE_LIMIT).every((s) => s === 201) && results.at(-1) === 429, results.join(','))
  const other = await register(newGroup())
  check('another address is unaffected', other.status === 201, String(other.status))
}

// 9. Deleting a group removes its data and only needs the key.
{
  const thief = await call('DELETE', '/v1/groups', { headers: { ...a.headers, Authorization: `Bearer ${b64url(randomBytes(32))}` } })
  check('deleting with the wrong token is refused', thief.status === 401, String(thief.status))
  check('and leaves the data in place', (await pull(a)).json.items.length === 3)
  const gone = await call('DELETE', '/v1/groups', { headers: a.headers })
  check('deleting with the right token succeeds', gone.status === 200, String(gone.status))
  check('the group is gone afterwards', (await pull(a)).status === 404)
  const reused = await register(a)
  check('and its id can be registered again from scratch', reused.status === 201 && (await pull(a)).json.items.length === 0, String(reused.status))
}

// 10. Routing.
{
  check('an unknown path is 404', (await call('GET', '/nope')).status === 404)
  check('an unsupported method is 405', (await call('PUT', '/v1/items', { headers: a.headers })).status === 405)
  check('the old sign-in endpoints no longer exist', (await call('GET', '/desktop/authorize')).status === 404)
}

console.log(failures === 0 ? '\nAll sync checks passed.' : `\n${failures} sync check(s) failed.`)
process.exitCode = failures === 0 ? 0 : 1

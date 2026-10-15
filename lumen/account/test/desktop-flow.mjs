// End-to-end check of the desktop login exchange against `npm run dev`.
// A real browser sign-in needs a Freebuff account, so this seeds a one-time
// code for a test user in local D1 (the same row desktopAuthorize writes) and
// then exercises the token, me, and sign-out endpoints.
import { createHash, randomBytes } from 'node:crypto'
import { execFileSync } from 'node:child_process'

const BASE = process.env.ACCOUNT_BASE ?? 'http://127.0.0.1:8787'
const REDIRECT = 'http://127.0.0.1:53111/callback'

const b64url = (buf) => buf.toString('base64url')
const sha256 = (s) => b64url(createHash('sha256').update(s).digest())

let failures = 0
function check(name, ok, detail = '') {
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ` (${detail})` : ''}`)
  if (!ok) failures++
}

function seedCode({ sub, code, challenge, redirectUri, expiresAt }) {
  const sql = `INSERT INTO desktop_codes (code_hash, sub, redirect_uri, code_challenge, expires_at) VALUES ('${sha256(code)}', '${sub}', '${redirectUri}', '${challenge}', ${expiresAt});`
  execFileSync('npx', ['wrangler', 'd1', 'execute', 'DB', '--local', '--command', sql], {
    cwd: new URL('..', import.meta.url),
    stdio: 'pipe',
  })
}

async function post(path, body, headers = {}) {
  return fetch(BASE + path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...headers },
    body: JSON.stringify(body),
    redirect: 'manual',
  })
}

const verifier = b64url(randomBytes(32))
const challenge = sha256(verifier)
const code = b64url(randomBytes(32))
const sub = `test-user-${b64url(randomBytes(6))}`

// 1. Signed-out authorize goes to Freebuff sign-in, keeping the request as returnTo.
{
  const q = new URLSearchParams({
    redirect_uri: REDIRECT,
    state: b64url(randomBytes(16)),
    code_challenge: challenge,
    code_challenge_method: 'S256',
  })
  const res = await fetch(`${BASE}/desktop/authorize?${q}`, { redirect: 'manual' })
  const location = res.headers.get('location') ?? ''
  check('authorize signed-out redirects to sign-in', res.status === 302 && location.includes('/auth/freebuff/signin?returnTo='), `${res.status} ${location.slice(0, 80)}`)
}

// 2. Authorize rejects a non-loopback redirect and a missing PKCE method.
{
  const q = new URLSearchParams({ redirect_uri: 'https://evil.example/callback', state: b64url(randomBytes(16)), code_challenge: challenge, code_challenge_method: 'S256' })
  const res = await fetch(`${BASE}/desktop/authorize?${q}`, { redirect: 'manual' })
  check('authorize rejects non-loopback redirect', res.status === 400, String(res.status))
}

// 3. Token exchange with the seeded code.
seedCode({ sub, code, challenge, redirectUri: REDIRECT, expiresAt: Date.now() + 60_000 })

{
  const res = await post('/api/desktop/token', { code, code_verifier: verifier, redirect_uri: REDIRECT })
  const body = await res.json().catch(() => ({}))
  check('token exchange returns an app token', res.status === 200 && typeof body.access_token === 'string', String(res.status))
  globalThis.TOKEN = body.access_token
}

// 4. The code is single use.
{
  const res = await post('/api/desktop/token', { code, code_verifier: verifier, redirect_uri: REDIRECT })
  check('replayed code is refused', res.status === 400, String(res.status))
}

// 5. The app token identifies the user.
{
  const res = await fetch(`${BASE}/api/desktop/me`, { headers: { Authorization: `Bearer ${globalThis.TOKEN}` } })
  const body = await res.json().catch(() => ({}))
  check('me with app token returns the sub', res.status === 200 && body.sub === sub, String(res.status))
}

// 6. Wrong verifier, expired code, and foreign Origin are refused.
{
  const code2 = b64url(randomBytes(32))
  const verifier2 = b64url(randomBytes(32))
  seedCode({ sub, code: code2, challenge: sha256(verifier2), redirectUri: REDIRECT, expiresAt: Date.now() + 60_000 })
  const bad = await post('/api/desktop/token', { code: code2, code_verifier: verifier, redirect_uri: REDIRECT })
  check('wrong PKCE verifier is refused', bad.status === 400, String(bad.status))
  const good = await post('/api/desktop/token', { code: code2, code_verifier: verifier2, redirect_uri: REDIRECT })
  check('code still usable after a wrong verifier', good.status === 200, String(good.status))

  const code3 = b64url(randomBytes(32))
  seedCode({ sub, code: code3, challenge: sha256(verifier), redirectUri: REDIRECT, expiresAt: Date.now() - 1000 })
  const expired = await post('/api/desktop/token', { code: code3, code_verifier: verifier, redirect_uri: REDIRECT })
  check('expired code is refused', expired.status === 400, String(expired.status))

  const browser = await post('/api/desktop/token', { code: code3, code_verifier: verifier, redirect_uri: REDIRECT }, { Origin: 'https://evil.example' })
  check('token endpoint refuses browser Origin', browser.status === 403, String(browser.status))
}

// 7. Sign-out revokes the app token.
{
  const out = await fetch(`${BASE}/api/desktop/signout`, { method: 'POST', headers: { Authorization: `Bearer ${globalThis.TOKEN}` } })
  check('sign-out succeeds', out.status === 200, String(out.status))
  const me = await fetch(`${BASE}/api/desktop/me`, { headers: { Authorization: `Bearer ${globalThis.TOKEN}` } })
  check('revoked token is refused', me.status === 401, String(me.status))
}

// 8. Without a token, /me is refused.
{
  const me = await fetch(`${BASE}/api/desktop/me`)
  check('me without token is refused', me.status === 401, String(me.status))
}

if (failures > 0) {
  console.log(`${failures} check(s) failed`)
  process.exit(1)
}
console.log('all desktop-flow checks passed')

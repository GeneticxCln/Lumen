import assert from "node:assert/strict";
import { resolveImageSrc } from "./remoteImage.ts";

const URL = "https://cdn.example/poster.jpg";

// Privacy confirmed off: load the raw URL directly, cache or not.
assert.deepEqual(resolveImageSrc(URL, false, undefined), { src: URL, failed: false });
assert.deepEqual(resolveImageSrc(URL, false, "data:cached"), { src: URL, failed: false }, "off always wins over a stale cache entry");

// Privacy confirmed on: only ever show a cached, guarded fetch - never the raw URL.
assert.deepEqual(resolveImageSrc(URL, true, "data:cached"), { src: "data:cached", failed: false });
assert.deepEqual(resolveImageSrc(URL, true, undefined), { src: undefined, failed: false }, "nothing to show until the guarded fetch completes");

// Not read yet (app just started): must behave exactly like "on", never like "off". This is
// the fail-closed default that closes the startup window where an unguarded image load could
// otherwise slip out before the saved setting is even read.
assert.deepEqual(
  resolveImageSrc(URL, null, undefined),
  { src: undefined, failed: false },
  "unknown privacy state must not fall back to loading the raw URL"
);
assert.deepEqual(
  resolveImageSrc(URL, null, "data:cached"),
  { src: "data:cached", failed: false },
  "a cache entry from before privacy state was known is still shown"
);

// No URL at all: nothing to show regardless of privacy state.
assert.deepEqual(resolveImageSrc(undefined, true, undefined), { src: undefined, failed: false });
assert.deepEqual(resolveImageSrc(undefined, false, undefined), { src: undefined, failed: false });
assert.deepEqual(resolveImageSrc(undefined, null, undefined), { src: undefined, failed: false });

console.log("remoteImage.test.ts: ok");

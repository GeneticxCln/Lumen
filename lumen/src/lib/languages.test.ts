import assert from "node:assert/strict";
import { clampScale, formatDelay, LANGUAGES, MAX_SUB_SCALE, MIN_SUB_SCALE } from "./languages.ts";

// Every offered code is one the backend accepts (two or three lowercase letters), and none repeats.
for (const { code } of LANGUAGES) assert.match(code, /^[a-z]{2,3}$/, code);
assert.equal(new Set(LANGUAGES.map((language) => language.code)).size, LANGUAGES.length);

assert.equal(clampScale(1.234), 1.23);
assert.equal(clampScale(0.1), MIN_SUB_SCALE);
assert.equal(clampScale(10), MAX_SUB_SCALE);
assert.equal(clampScale(1.1 + 0.1 + 0.1), 1.3, "float noise from stepping is rounded away");

assert.equal(formatDelay(0), "0 s");
assert.equal(formatDelay(0.5), "+0.5 s");
assert.equal(formatDelay(-1), "-1.0 s");

console.log("Language checks passed; offered codes match the backend and sizes stay in range.");

import assert from "node:assert/strict";
import { addLocale, availableLocales, setLocale, splitRich, t, tn, tRich } from "./index.ts";
import type { Catalog } from "./index.ts";
import { en } from "./en.ts";

// Interpolation.
assert.equal(t("common.cancel"), "Cancel");
assert.equal(t("modal.close", { title: "Settings" }), "Close Settings");
assert.equal(t("episodes.watchedCount", { watched: 2, total: 10 }), "2 of 10 watched");
assert.equal(t("episodes.row", { episode: "03", name: "Pilot" }), "E03 · Pilot");

// Plurals follow the language's rules and expose {count}.
assert.equal(tn("sync.pending", 1), "1 change waiting to upload.");
assert.equal(tn("sync.pending", 3), "3 changes waiting to upload.");
assert.equal(tn("sync.pending", 0), "0 changes waiting to upload.");
assert.match(tn("sync.skipped", 1), /^1 synced record could not be verified and was ignored/);
assert.match(tn("sync.skipped", 4), /^4 synced records could not be verified and were ignored/);

// A placeholder the caller did not supply stays visible instead of vanishing.
assert.equal(t("modal.close", {} as never), "Close {title}");

// Markup is split out for the component to render.
assert.deepEqual(splitRich("a <code>b</code> c <code>d</code>"), [
  { text: "a " },
  { tag: "code", text: "b" },
  { text: " c " },
  { tag: "code", text: "d" },
]);
assert.deepEqual(splitRich("plain"), [{ text: "plain" }]);
assert.deepEqual(
  tRich("prefs.sideloadNote").filter((part) => part.tag).map((part) => part.text),
  ["Movie.srt", "Movie.mkv"],
);

// A language is chosen by code; `de-AT` falls back to `de`, and anything unknown to English.
// This table is a test fixture, not a real translation: every key is the English text with a marker.
const marked = Object.fromEntries(Object.entries(en).map(([key, value]) => [key, `«${value}»`])) as Catalog;
const german: Catalog = { ...marked, "common.cancel": "Abbrechen", "sync.pending.one": "{count} Änderung wartet.", "sync.pending.other": "{count} Änderungen warten." };
addLocale("de", german);
assert.ok(availableLocales().includes("de"));

setLocale("de-AT");
assert.equal(t("common.cancel"), "Abbrechen");
assert.equal(tn("sync.pending", 1), "1 Änderung wartet.");
assert.equal(tn("sync.pending", 2), "2 Änderungen warten.");

setLocale("xx");
assert.equal(t("common.cancel"), "Cancel", "an unknown language falls back to English");
setLocale("en");
assert.equal(t("common.cancel"), "Cancel");

// A key missing from a language at run time (an old translation file) falls back to English.
addLocale("fr", { "common.cancel": "Annuler" } as unknown as Catalog);
setLocale("fr");
assert.equal(t("common.cancel"), "Annuler");
assert.equal(t("common.loading"), "Loading…");
setLocale("en");

// Every key is spelled consistently: plural keys come in pairs, and no message mixes in stray braces.
const keys = Object.keys(en);
for (const key of keys) {
  if (key.endsWith(".one")) assert.ok(keys.includes(`${key.slice(0, -4)}.other`), `${key} needs a .other form`);
  if (key.endsWith(".other")) assert.ok(keys.includes(`${key.slice(0, -6)}.one`), `${key} needs a .one form`);
  const text = (en as Record<string, string>)[key];
  assert.equal(text.replace(/\{\w+\}/g, "").match(/[{}]/), null, `${key} has an unmatched brace`);
}

console.log("i18n.test.ts: ok");

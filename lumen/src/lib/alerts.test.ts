import assert from "node:assert/strict";
import { alertKey, newEpisodeAlerts, rememberNotified } from "./alerts.ts";
import type { UpcomingSeries } from "./series.ts";

const series = (patch: Partial<UpcomingSeries>): UpcomingSeries => ({
  tmdbId: 1, name: "Severance", status: "Returning Series", nextAirDate: null, nextSeason: null, nextEpisode: null,
  nextName: null, lastAirDate: "2026-10-15", lastSeason: 2, lastEpisode: 4, channelId: null, ...patch,
});
const today = "2026-10-16";
const none = new Set<string>();

assert.deepEqual(newEpisodeAlerts([series({})], today, none).map((a) => a.key), [alertKey(1, 2, 4)], "yesterday's episode is new");
assert.equal(newEpisodeAlerts([series({ lastAirDate: today })], today, none).length, 1, "today's too");
assert.equal(newEpisodeAlerts([series({ lastAirDate: "2026-10-13" })], today, none).length, 1, "three days is still new");
assert.equal(newEpisodeAlerts([series({ lastAirDate: "2026-10-12" })], today, none).length, 0, "older is not");
assert.equal(newEpisodeAlerts([series({ lastAirDate: "2026-10-20" })], today, none).length, 0, "the future is not");
assert.equal(newEpisodeAlerts([series({ lastAirDate: null })], today, none).length, 0);
assert.equal(newEpisodeAlerts([series({ lastEpisode: null })], today, none).length, 0);
assert.equal(newEpisodeAlerts([series({})], today, new Set([alertKey(1, 2, 4)])).length, 0, "announced once");
assert.equal(newEpisodeAlerts([series({ lastEpisode: 5 })], today, new Set([alertKey(1, 2, 4)])).length, 1, "the next episode is announced again");

const many = [
  series({ tmdbId: 1, name: "B Show", lastAirDate: "2026-10-14" }),
  series({ tmdbId: 2, name: "A Show", lastAirDate: "2026-10-15" }),
  series({ tmdbId: 3, name: "C Show", lastAirDate: "2026-10-15" }),
];
assert.deepEqual(newEpisodeAlerts(many, today, none).map((a) => a.seriesName), ["A Show", "C Show", "B Show"], "newest first, then by name");

assert.deepEqual(rememberNotified(["a", "b"], ["c"]), ["a", "b", "c"]);
assert.deepEqual(rememberNotified(["a", "b"], ["a"]), ["b", "a"], "no duplicates");
assert.deepEqual(rememberNotified(["a", "b", "c"], ["d"], 2), ["c", "d"], "the oldest are dropped");

console.log("alerts.test.ts: ok");

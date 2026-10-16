import assert from "node:assert/strict";
import { buildBoard, isFinished, isInProgress, remainingLabel, ROW_LIMIT } from "./board.ts";
import type { Channel, HistoryEntry } from "./types.ts";

function ch(id: number, name: string, extra: Partial<Channel> = {}): Channel {
  return { id, playlist_id: 1, name, url: `https://example.org/${id}`, group_title: "Movies", is_live: false, ...extra };
}
let historyId = 0;
function hist(channel: Channel | null, position: number, duration: number, playedAt: number, path?: string): HistoryEntry {
  return { id: ++historyId, channel_id: channel?.id, path: path ?? channel!.url, position, duration, played_at: playedAt };
}

const movie = ch(1, "Heat");
const unwatched = ch(2, "Ronin");
const e1 = ch(10, "Severance S01E01 Good News");
const e2 = ch(11, "Severance S01E02 Half Loop");
const e3 = ch(12, "Severance S01E03 In Perpetuity");
const other1 = ch(20, "Fargo S01E01 The Crocodile");
const other2 = ch(21, "Fargo S01E02 The Rooster");
const liveNews = ch(30, "World News", { group_title: "News", is_live: true });
const library = [movie, unwatched, e1, e2, e3, other1, other2, liveNews];
const base = { channels: library, favorites: new Set<number>(), newChannelIds: [], recommended: library };

// Thresholds
assert.equal(isInProgress(hist(movie, 600, 7200, 1)), true);
assert.equal(isInProgress(hist(movie, 5, 7200, 1)), false, "a few seconds is not something to resume");
assert.equal(isInProgress(hist(movie, 7100, 7200, 1)), false, "98% played is finished");
assert.equal(isFinished(hist(movie, 7100, 7200, 1)), true);
assert.equal(isInProgress(hist(movie, 100, 0, 1)), false, "unknown duration cannot be resumed");

// Continue watching: in-progress titles, most recent first, with progress
{
  const board = buildBoard({ ...base, history: [hist(movie, 1800, 7200, 100), hist(unwatched, 3600, 7200, 200)] });
  assert.deepEqual(board.continueWatching.map((item) => item.channel?.id), [2, 1], "most recently played first");
  assert.equal(board.continueWatching[0].progress, 0.5);
  assert.equal(board.continueWatching[0].remainingSeconds, 3600);
  assert.ok(board.recommended.every((channel) => channel.id !== 1 && channel.id !== 2), "titles being continued are not recommended again");
}

// A finished movie is neither continued nor suggested as next up
{
  const board = buildBoard({ ...base, history: [hist(movie, 7100, 7200, 100)] });
  assert.equal(board.continueWatching.length, 0);
  assert.equal(board.nextUp.length, 0);
}

// Next up: finishing an episode suggests the following one, once per series
{
  const board = buildBoard({ ...base, history: [hist(e1, 3000, 3100, 100), hist(other1, 2900, 3000, 200)] });
  assert.deepEqual(board.nextUp.map((item) => [item.channel.id, item.after]), [[21, other1.name], [11, e1.name]], "newest finish first");
}
// ...but not when the next episode was already started or watched
{
  const board = buildBoard({ ...base, history: [hist(e1, 3000, 3100, 100), hist(e2, 600, 3000, 300)] });
  assert.equal(board.nextUp.length, 0, "the series is being resumed, not advanced");
  assert.deepEqual(board.continueWatching.map((item) => item.channel?.id), [11]);
}
// The latest entry decides: finished E1 then finished E2 suggests E3 only
{
  const board = buildBoard({ ...base, history: [hist(e1, 3000, 3100, 100), hist(e2, 3000, 3100, 200)] });
  assert.deepEqual(board.nextUp.map((item) => item.channel.id), [12]);
}
// A series with several in-progress episodes shows only the latest
{
  const board = buildBoard({ ...base, history: [hist(e1, 600, 3000, 100), hist(e2, 700, 3000, 200)] });
  assert.deepEqual(board.continueWatching.map((item) => item.channel?.id), [11]);
}

// Files on this computer continue by path; stale stream history with no library entry is dropped
{
  const board = buildBoard({
    ...base,
    history: [
      hist(null, 120, 600, 300, "/home/q/Videos/Holiday 2024.mp4"),
      hist(null, 120, 600, 250, "https://gone.example/removed.m3u8"),
    ],
  });
  assert.equal(board.continueWatching.length, 1);
  assert.equal(board.continueWatching[0].path, "/home/q/Videos/Holiday 2024.mp4");
  assert.equal(board.continueWatching[0].title, "Holiday 2024");
}

// Live channels never continue
{
  const board = buildBoard({ ...base, history: [hist(liveNews, 400, 3600, 100)] });
  assert.equal(board.continueWatching.length, 0);
}

// New since last visit: unwatched, non-live, in the order the backend gave, no duplicates with other rows
{
  const board = buildBoard({
    ...base,
    history: [hist(e1, 3000, 3100, 100), hist(movie, 600, 7200, 50)],
    newChannelIds: [30, 1, 11, 2, 999, 2],
  });
  assert.deepEqual(board.fresh.map((channel) => channel.id), [2], "live, watched, already-suggested and unknown entries are left out");
}

// My List keeps library order; recommended drops what is already on the board
{
  const board = buildBoard({ ...base, history: [hist(movie, 600, 7200, 50)], favorites: new Set([21, 2]) });
  assert.deepEqual(board.myList.map((channel) => channel.id), [2, 21]);
  assert.ok(board.recommended.every((channel) => channel.id !== 1), "a title being continued is not also recommended");
  assert.ok(board.recommended.some((channel) => channel.id === 2));
}

// Rows are capped
{
  const many = Array.from({ length: 80 }, (_, index) => ch(100 + index, `Title ${index}`));
  const board = buildBoard({ channels: many, history: [], favorites: new Set(many.map((c) => c.id)), newChannelIds: many.map((c) => c.id), recommended: many });
  assert.equal(board.fresh.length, ROW_LIMIT);
  assert.equal(board.myList.length, ROW_LIMIT);
  assert.equal(board.recommended.length, ROW_LIMIT);
}

// Does not mutate inputs
{
  const history = [hist(movie, 600, 7200, 50), hist(unwatched, 600, 7200, 60)];
  const copy = JSON.stringify(history);
  buildBoard({ ...base, history });
  assert.equal(JSON.stringify(history), copy);
}

assert.equal(remainingLabel(30), "1 min left");
assert.equal(remainingLabel(12 * 60), "12 min left");
assert.equal(remainingLabel(3600), "1 h left");
assert.equal(remainingLabel(3900), "1 h 5 min left");

console.log("Board checks passed; continue watching, next up, new since last visit and row limits behave.");

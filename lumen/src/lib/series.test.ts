import assert from "node:assert/strict";
import { buildSeasons, describeUpcoming, finishedChannelIds, indexMarks, isEpisodeWatched, seriesIdOf } from "./series.ts";
import type { EpisodeMark, UpcomingSeries } from "./series.ts";
import type { Channel, HistoryEntry } from "./types.ts";

function ch(id: number, name: string, extra: Partial<Channel> = {}): Channel {
  return { id, playlist_id: 1, name, url: `https://example.org/${id}`, group_title: "Series", ...extra };
}
const mark = (seriesKey: string, season: number, episode: number, watched: boolean, updatedAt = 1): EpisodeMark => ({ seriesKey, season, episode, watched, updatedAt });
let hid = 0;
const hist = (channel: Channel, position: number, duration: number): HistoryEntry => ({ id: ++hid, channel_id: channel.id, path: channel.url, position, duration, played_at: 1 });

// Series identity: TMDB id when matched, normalised name otherwise, nothing for non-episodes
assert.equal(seriesIdOf(ch(1, "Severance S01E01 Good News")), "name:severance");
assert.equal(seriesIdOf(ch(2, "Severance 1x02", { tmdb_type: "tv", tmdb_id: 95396 })), "tmdb:tv:95396");
assert.equal(seriesIdOf(ch(3, "Heat", { tmdb_type: "movie", tmdb_id: 949 })), null);

const e11 = ch(11, "Severance S01E01 Good News");
const e12 = ch(12, "Severance S01E02 Half Loop");
const e12b = ch(13, "Severance S01E02 Half Loop (backup)", { last_error: "HTTP 404" });
const e13 = ch(14, "Severance S01E03 In Perpetuity");
const e21 = ch(15, "Severance S02E01 Hello, Ms. Cobel");
const other = ch(16, "Fargo S01E01 The Crocodile");
const library = [e21, e13, e12b, e12, e11, other];

// Watched: a mark wins over history, history otherwise
const finished = finishedChannelIds([hist(e11, 3000, 3100), hist(e12, 100, 3100)], library);
assert.deepEqual([...finished], [11], "only a finished playback counts");
const marks = indexMarks([mark("name:severance", 1, 1, false), mark("name:severance", 1, 3, true)]);
assert.equal(isEpisodeWatched(e11, marks, finished), false, "marked unwatched overrides a finished playback");
assert.equal(isEpisodeWatched(e13, marks, finished), true, "marked watched with no playback");
assert.equal(isEpisodeWatched(e12, marks, finished), false, "no mark and not finished");
assert.equal(isEpisodeWatched(ch(99, "Heat"), marks, finished), false);

// Seasons: grouped, ordered, one row per episode, a working source preferred, counts right
{
  const seasons = buildSeasons(e12, library, [mark("name:severance", 1, 3, true)], [hist(e11, 3000, 3100)]);
  assert.deepEqual(seasons.map((s) => s.season), [1, 2]);
  assert.deepEqual(seasons[0].episodes.map((r) => [r.episode, r.watched]), [[1, true], [2, false], [3, true]]);
  assert.equal(seasons[0].watchedCount, 2);
  assert.equal(seasons[0].episodes[1].channel.id, 12, "the working source of E02 is kept over the broken backup");
  assert.equal(seasons[1].episodes.length, 1);
  assert.ok(!seasons.flatMap((s) => s.episodes).some((r) => r.channel.id === 16), "another series is not included");
}
assert.deepEqual(buildSeasons(ch(99, "Heat"), library, [], []), [], "a non-episode has no seasons");
// The entry being viewed wins even if it is the broken one
assert.equal(buildSeasons(e12b, library, [], [])[0].episodes[1].channel.id, 13);

// Upcoming labels
const base: UpcomingSeries = { tmdbId: 1, name: "Show", status: null, nextAirDate: null, nextSeason: null, nextEpisode: null, nextName: null, lastAirDate: null, lastSeason: null, lastEpisode: null, channelId: null };
const today = "2026-10-16";
assert.deepEqual(describeUpcoming({ ...base, nextAirDate: "2026-10-16", nextSeason: 3, nextEpisode: 4 }, today), { kind: "upcoming", text: "S03E04 airs today" });
assert.equal(describeUpcoming({ ...base, nextAirDate: "2026-10-17", nextSeason: 3, nextEpisode: 4 }, today)!.text, "S03E04 airs tomorrow");
assert.equal(describeUpcoming({ ...base, nextAirDate: "2026-10-20", nextSeason: 3, nextEpisode: 4 }, today)!.text, "S03E04 airs in 4 days");
assert.equal(describeUpcoming({ ...base, nextAirDate: "2026-11-01", nextSeason: 3, nextEpisode: 4 }, today)!.text, "S03E04 airs Nov 1");
assert.deepEqual(describeUpcoming({ ...base, lastAirDate: "2026-10-14", lastSeason: 3, lastEpisode: 3 }, today), { kind: "aired", text: "S03E03 aired 2 days ago" });
assert.equal(describeUpcoming({ ...base, lastAirDate: "2026-10-15", lastSeason: 3, lastEpisode: 3 }, today)!.text, "S03E03 aired yesterday");
assert.equal(describeUpcoming({ ...base, lastAirDate: "2026-09-01", lastSeason: 3, lastEpisode: 3 }, today), null, "old news is not shown");
assert.equal(describeUpcoming({ ...base, nextAirDate: "2026-10-10", lastAirDate: "2026-10-03", lastSeason: 2, lastEpisode: 9 }, today)!.text, "S02E09 aired 13 days ago", "a stale next date falls back to the last aired episode");
assert.equal(describeUpcoming(base, today), null);

console.log("Series checks passed; marks override history, seasons group, airing labels read naturally.");

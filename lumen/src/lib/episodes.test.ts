import assert from "node:assert/strict";
import { episodeInfo, nextEpisode } from "./episodes.ts";
import type { Channel } from "./types.ts";

function show(id: number, name: string, extra: Partial<Channel> = {}): Channel {
  return { id, playlist_id: 1, name, url: `https://example.org/${id}`, group_title: "Series", ...extra };
}

assert.deepEqual(episodeInfo(show(1, "Breaking Bad S01E02 Cat's in the Bag")), { seriesKey: "breaking bad", season: 1, episode: 2 }, "SxxExx markers are parsed");
assert.deepEqual(episodeInfo(show(2, "Breaking Bad - S1 E10")), { seriesKey: "breaking bad", season: 1, episode: 10 }, "separators before the marker are dropped");
assert.deepEqual(episodeInfo(show(3, "The Wire 2x05")), { seriesKey: "the wire", season: 2, episode: 5 }, "NxNN markers are parsed");
assert.equal(episodeInfo(show(4, "Big Buck Bunny 1080x720")), null, "resolutions are not episode markers");
assert.equal(episodeInfo(show(5, "Mountain Life")), null, "titles without a marker are not episodes");
assert.equal(episodeInfo(show(6, "S01E01 Pilot")), null, "a marker with no series name cannot be matched");

const library: Channel[] = [
  show(10, "Breaking Bad S01E01 Pilot"),
  show(11, "Breaking Bad S01E02 Cat's in the Bag"),
  show(12, "Breaking Bad S01E03 ...And the Bag's in the River"),
  show(13, "Breaking Bad S02E01 Seven Thirty-Seven"),
  show(14, "Better Call Saul S01E02 Mijo"),
  show(15, "Breaking Bad S01E04 Cancer Man", { last_error: "HTTP 404" }),
  show(16, "Breaking Bad S01E05 Gray Matter"),
];

const first = library[0];
assert.equal(nextEpisode(first, library)?.id, 11, "the next episode in the same season follows");
assert.equal(nextEpisode(library[1], library)?.id, 12, "the closest episode after the current one is chosen");
assert.equal(nextEpisode(library[2], [library[2], library[3]])?.id, 13, "the next season's first episode follows the last of the previous season");
assert.equal(nextEpisode(library[2], library.slice(0, 3)), null, "the last known episode has no successor");
assert.equal(nextEpisode(library[4], library), null, "another series does not count as the next episode");
assert.equal(nextEpisode(library[2], library)?.id, 16, "a broken title (last_error) is skipped, so E03 is followed by E05");
assert.equal(nextEpisode(show(20, "Mountain Life"), library), null, "non-episode titles have no next episode");

console.log("Episode checks passed; next episode follows series, season and episode order.");

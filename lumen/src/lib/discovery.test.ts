import { filterChannels, recommendChannels, uniqueHistory, readPreferences, artworkCategory, themes, resolveTheme, contentKind, inferGenre, inferGenres, groupsOf, genresOf, yearOf, matchesFacets, buildFacets, facetCount, catalogShelves, isCatalogTitle, browseCaption, EMPTY_FACETS, ALL_FACETS, UNKNOWN_FACET } from "./discovery.ts";
import type { FacetValue } from "./discovery.ts";
import type { Channel, HistoryEntry } from "./types.ts";

function assert(value: unknown, message: string): asserts value { if (!value) throw new Error(message); }
const channels: Channel[] = [
  { id: 1, playlist_id: 1, name: "World News", url: "https://example.org/news", group_title: "News" },
  { id: 2, playlist_id: 2, name: "Mountain Life", url: "https://example.org/mountain", group_title: "Documentary" },
  { id: 3, playlist_id: 1, name: "Science Today", url: "https://example.org/science", group_title: "Science", last_error: "HTTP 404" },
  { id: 4, playlist_id: 2, name: "Another World", url: "https://example.org/another", group_title: "News" },
];
const history: HistoryEntry[] = [
  { id: 1, path: channels[0].url, channel_id: 1, position: 12, duration: 100, played_at: 10 },
  { id: 2, path: channels[0].url, channel_id: 1, position: 50, duration: 100, played_at: 20 },
  { id: 3, path: "/video.mp4", position: 15, duration: 120, played_at: 20 },
];
assert(filterChannels(channels, "WORLD news", "all", false).length === 2, "Search includes title/category terms across sources");
assert(filterChannels(channels, "", "Science", true).length === 0, "Unavailable filter excludes failed checks");
assert(filterChannels(channels, "", "all", true).length === 3, "Unchecked streams stay visible");
assert(filterChannels(channels, "", "Other", false).length === 0, "Unknown category should not match existing groups");
const recent = uniqueHistory(history);
assert(recent.length === 2 && recent[0].id === 3 && recent[1].position === 50, "History is unique and latest entry wins, including timestamp ties");
const picks = recommendChannels(channels, history, new Set([2]));
assert(picks[0].id === 2, "Favorites influence ranking");
assert(!picks.some((channel) => channel.id === 3), "Unavailable titles are never recommended");
assert(recommendChannels(channels, history, new Set())[0].id === 4, "Recent categories prioritize unwatched titles");
assert(channels[0].id === 1 && history[0].id === 1, "Discovery does not mutate input");
assert(artworkCategory("Sports") === "adventure", "Artwork categories map correctly");
let stored = '{"compactCards":true,"defaultVolume":150,"reduceMotion":"yes"}';
Object.defineProperty(globalThis, "localStorage", { value: { getItem: () => stored }, configurable: true });
assert(readPreferences().compactCards && readPreferences().defaultVolume === 100 && !readPreferences().reduceMotion, "Preferences validate types and clamp volume");
assert(readPreferences().theme === "cinema", "Old preferences without a theme keep the Cinema default");
for (const theme of themes) {
  stored = JSON.stringify({ theme: theme.id });
  assert(readPreferences().theme === theme.id, `Theme ${theme.id} persists`);
}
stored = '{"theme":"invalid-theme"}';
assert(readPreferences().theme === "cinema", "Unsupported themes fall back safely");
assert(resolveTheme("system", true) === "cinema", "System theme resolves dark mode");
assert(resolveTheme("system", false) === "light", "System theme resolves light mode");
assert(resolveTheme("forest", false) === "forest", "Explicit themes ignore system mode");
stored = "not json";
assert(readPreferences().defaultVolume === 80, "Malformed preferences recover to defaults");
const large = Array.from({ length: 10000 }, (_, index) => ({ ...channels[index % channels.length], id: index + 1, name: `Title ${index}` }));
const start = performance.now();
const results = recommendChannels(filterChannels(large, "title", "all", true), history, new Set([2]));
assert(results.length === 7500, "10k-entry filtering and ranking retain all available items");
console.log(`Discovery regression checks passed; 10,000 entries filtered + ranked in ${(performance.now() - start).toFixed(2)}ms.`);

// --- Categories: Group, Genre, Year ---------------------------------------
// Real shape of the library: TMDB-matched movies alongside IPTV channels whose
// group strings pack several categories into one field.
const categorised: Channel[] = [
  { id: 101, playlist_id: 1, name: "Big Buck Bunny", url: "u1", group_title: "Movies", tmdb_id: 10378, tmdb_type: "movie", release_year: 2008, rating: 7.2, genre_names: "Animation;Family;Comedy", poster_path: "/p1.jpg" },
  { id: 102, playlist_id: 1, name: "Tears of Steel", url: "u2", group_title: "Movies", tmdb_id: 133701, tmdb_type: "movie", release_year: 2012, rating: 6.2, genre_names: "Science Fiction;Action" },
  { id: 103, playlist_id: 2, name: "BFM Business (1080p)", url: "u3", group_title: "Business;News" },
  { id: 104, playlist_id: 2, name: "ZillarBarta News", url: "u4", group_title: "Classic;News" },
  { id: 105, playlist_id: 2, name: "Mystery Feed", url: "u5", group_title: "Unmapped" },
  { id: 106, playlist_id: 2, name: "No tags", url: "u6" },
];

assert(inferGenre("Business;News") === "Business", "The legacy single-genre helper returns the first recognized group genre");
assert(inferGenres("Culture;Entertainment;Music;News").join(",") === "Culture,Entertainment,Music,News", "Every packed group category is exposed as a genre fallback");
assert(groupsOf(" Business ; News ; Business ").join(",") === "Business,News", "Group tags are trimmed and deduplicated");
assert(inferGenre("Sports") === "Sports", "Single-token groups map directly");
assert(inferGenre("Unmapped") === "Unmapped", "Unrecognized group names remain reachable as fallback genres");
assert(inferGenres().join(",") === "Uncategorized", "A title without tags still receives a genre category");
assert(groupsOf().join(",") === "Other", "A title without a group remains reachable through Group");

assert(genresOf(categorised[0]).source === "tmdb" && genresOf(categorised[0]).names.length === 3, "TMDB genres win when present");
assert(genresOf(categorised[2]).source === "inferred" && genresOf(categorised[2]).names[0] === "Business", "Live channels fall back to an inferred genre");
assert(genresOf(categorised[4]).names[0] === "Unmapped", "An unfamiliar source group stays reachable through Genre");
assert(genresOf(categorised[5]).names[0] === "Uncategorized", "A title without source tags gets an explicit fallback genre");

// --- Manual category override: a correction TMDB never matched ------------
const overridden = { ...categorised[2], category_override_kind: "series" as const, category_override_genres: "Comedy;Family" };
assert(genresOf(overridden).source === "override" && genresOf(overridden).names.join(",") === "Comedy,Family", "A saved override wins over the inferred guess");
assert(genresOf({ ...categorised[0], category_override_genres: "Comedy" }).source === "tmdb", "TMDB stays authoritative over an override that should never have been saved for a matched title");
assert(contentKind(overridden) === "series", "A saved Kind override wins over the group/name guess");
assert(contentKind({ ...categorised[0], category_override_kind: "series" }) === "movie", "TMDB's own media type still wins over an override");

assert(contentKind(categorised[0]) === "movie" && contentKind({ ...categorised[0], tmdb_type: "tv" }) === "series", "TMDB media type classifies enriched titles for the right browse layout");
assert(contentKind({ ...categorised[0], tmdb_type: undefined, group_title: "Movies" }) === "movie", "Unmatched titles retain the source-name classification fallback");
const plain: Channel = { id: 90, playlist_id: 1, name: "Channel 9", url: "https://example.org/9", group_title: "General" };
assert(contentKind(plain) === "live", "An unlabelled entry with no duration is treated as live");
assert(contentKind({ ...plain, is_live: true }) === "live", "-1 says nothing more than the default");
assert(contentKind({ ...plain, is_live: false }) === "movie", "A fixed-length entry with no other hint is on-demand, not live");
assert(contentKind({ ...plain, is_live: true, group_title: "Movies" }) === "movie", "Providers that write -1 for films keep their group-name classification");
assert(contentKind({ ...plain, is_live: false, name: "Show S01E02" }) === "series", "Episode names still win for on-demand entries");
assert(yearOf(categorised[0]) === 2008, "TMDB release year is used");
assert(yearOf(categorised[2]) === null, "A live channel has no year");
assert(yearOf({ ...categorised[0], release_year: 0 }) === null, "A zero year is treated as unknown");

assert(matchesFacets(categorised[0], { ...EMPTY_FACETS, genre: "Animation" }), "Genre matching uses TMDB names");
assert(!matchesFacets(categorised[2], { ...EMPTY_FACETS, genre: "Animation" }), "Genre matching excludes other titles");
assert(matchesFacets(categorised[5], { ...EMPTY_FACETS, genre: "Uncategorized" }), "An untagged title is in an explicit Genre bucket");
assert(!matchesFacets(categorised[0], { ...EMPTY_FACETS, genre: UNKNOWN_FACET }), "A TMDB-matched title is not in the unknown-genre bucket");
assert(matchesFacets(categorised[3], { ...EMPTY_FACETS, year: UNKNOWN_FACET }), "Live channels land under an unknown year");
assert(matchesFacets(categorised[0], { ...EMPTY_FACETS, year: "2008" }), "Year matching works on TMDB years");
assert(facetCount(EMPTY_FACETS) === 0 && facetCount({ group: "Movies", genre: "Animation", year: ALL_FACETS }) === 2, "Applied-category count reports only active facets");

const all = buildFacets(categorised, EMPTY_FACETS);
const sumOf = (values: FacetValue[]) => values.filter((value) => value.value !== ALL_FACETS).reduce((total, value) => total + value.count, 0);

// The requirement: a category exists for every group, genre and year, so no title
// can become unreachable by browsing.
assert(categorised.every((channel) => all.groups.some((value) => value.value !== ALL_FACETS && matchesFacets(channel, { ...EMPTY_FACETS, group: value.value }))), "Every title is reachable through the Group facet");
assert(all.groups.some((value) => value.value === "Business") && all.groups.some((value) => value.value === "News"), "Every semicolon-delimited group tag gets its own facet");
assert(matchesFacets(categorised[2], { ...EMPTY_FACETS, group: "Business" }) && matchesFacets(categorised[2], { ...EMPTY_FACETS, group: "News" }), "A packed group title is reachable through each group tag");
assert(categorised.every((channel) => all.genres.some((value) => value.value !== ALL_FACETS && matchesFacets(channel, { ...EMPTY_FACETS, genre: value.value }))), "Every title is reachable through the Genre facet");
assert(categorised.every((channel) => all.years.some((value) => value.value !== ALL_FACETS && matchesFacets(channel, { ...EMPTY_FACETS, year: value.value }))), "Every title is reachable through the Year facet");

assert(sumOf(all.groups) >= categorised.length, "Overlapping group tags preserve coverage without dropping titles");
assert(sumOf(all.years) === categorised.length, "Year categories partition the library with no gaps");
assert(all.groups[0].count === categorised.length, "The Group 'All' count is the whole set");
assert(all.genres.some((value) => value.value === "Uncategorized" && value.count === 1), "Untagged titles have a visible genre fallback");
assert(all.genres.some((value) => value.value === "News" && value.count === 2), "Genre fallback includes every tag in packed source groups");
assert(all.years.some((value) => value.value === UNKNOWN_FACET && value.count === 4), "Titles without a TMDB release year share the explicit unknown-year bucket");
assert(all.years.filter((value) => value.value !== ALL_FACETS).map((value) => value.value).join(",") === `2012,2008,${UNKNOWN_FACET}`, "Years sort newest first with the unknown bucket last");
assert(all.hasTmdbGenres && all.enriched === 2, "Facets report TMDB coverage honestly");

// Counts are computed inside the other two facets, so a chip's number is what
// choosing it would actually show.
const inMovies = buildFacets(categorised, { ...EMPTY_FACETS, group: "Movies" });
assert(inMovies.genres[0].count === 2, "Genre counts narrow to the selected group");
assert(!inMovies.genres.some((value) => value.label === "Business"), "A genre absent from the selection is not offered");
assert(inMovies.groups[0].count === categorised.length, "The Group facet still counts across the whole set");
const inNews = buildFacets(categorised, { ...EMPTY_FACETS, group: "News" });
assert(inNews.groups[0].count === categorised.length && inNews.years[0].count === 2, "Selecting any packed group tag filters titles while counts remain conditioned");
assert(buildFacets(categorised, { ...EMPTY_FACETS, genre: "News" }).genres.find((value) => value.value === "News")?.count === 2, "Genre counts reflect every title in an inferred group tag");
assert(buildFacets(categorised, { ...EMPTY_FACETS, group: "Movies", genre: "Animation" }).years[0].count === 1, "Narrowing two facets narrows the third");

// --- Discovery shelves: the TMDB catalogue -----------------------------------
const matched = (id: number, name: string, genres: string, rating: number, year: number): Channel => ({
  id, playlist_id: 1, name, url: `https://example.org/${id}`, group_title: "Movies", tmdb_id: id, tmdb_type: "movie", genre_names: genres, rating, release_year: year,
});
const catalogue: Channel[] = [
  matched(1, "Alpha", "Drama;Crime", 7.1, 2001),
  matched(2, "Bravo", "Drama", 8.4, 2019),
  matched(3, "Charlie", "Comedy", 6.0, 2023),
  matched(4, "Delta", "Drama", 7.7, 2010),
  { id: 5, playlist_id: 2, name: "Unmatched News", url: "https://example.org/news", group_title: "News", rating: 9.9, release_year: 2025 },
  { id: 6, playlist_id: 2, name: "Unmatched Drama", url: "https://example.org/drama", group_title: "Drama" },
];
assert(!isCatalogTitle(catalogue[4]) && isCatalogTitle(catalogue[0]), "Only TMDB-matched titles are catalogue titles");
const shelves = catalogShelves(catalogue);
const shelfTitles = shelves.map((shelf) => shelf.title);
assert(shelfTitles.join("|") === "Top rated|Recently released|Drama", `Catalogue shelves are Top rated, Recently released and genre shelves with 2+ titles (got ${shelfTitles.join("|")})`);
assert(!shelves.some((shelf) => shelf.channels.some((channel) => channel.id >= 5)), "Unmatched playlist entries never appear on catalogue shelves");
assert(shelves[0].channels.map((channel) => channel.name).join(",") === "Bravo,Delta,Alpha,Charlie", "Top rated sorts by TMDB rating, highest first");
assert(shelves[1].channels[0].name === "Charlie" && shelves[1].channels[shelves[1].channels.length - 1].name === "Alpha", "Recently released sorts newest first");
assert(shelves[2].channels.map((channel) => channel.name).join(",") === "Bravo,Delta,Alpha", "Genre shelves hold every title with that TMDB genre, best rated first");
assert(catalogShelves([]).length === 0 && catalogShelves(channels).length === 0, "No TMDB-matched titles means no catalogue shelves");
const many = Array.from({ length: 30 }, (_, index) => matched(100 + index, `Title ${index}`, "Action", index / 10, 2000 + index));
const capped = catalogShelves(many);
assert(capped.every((shelf) => shelf.channels.length <= 20), "Each catalogue shelf is capped at 20 titles");
assert(capped.find((shelf) => shelf.title === "Action")?.channels[0].name === "Title 29", "The capped shelf keeps the best-rated titles");

// --- Discovery browse titles: TMDB trending and top-rated, not in the library -----
assert(browseCaption({ tmdb_id: 1, tmdb_type: "movie", name: "Film", release_year: 2008, rating: 7.2, genre_names: "Animation;Family;Comedy" }) === "2008 · ★ 7.2 · Animation · Family", "Browse captions show year, rating and up to two genres");
assert(browseCaption({ tmdb_id: 2, tmdb_type: "tv", name: "Show", release_year: null, rating: 0, genre_names: null }) === "On TMDB", "Browse captions fall back when TMDB gave no metadata");

console.log("Category facet checks passed; every title reachable via Group, Genre and Year.");
console.log("Discovery shelf checks passed.");

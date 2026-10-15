import type { Channel, HistoryEntry } from "./types.ts";

export const themes = [
  { id: "cinema", name: "Cinema", description: "The original. Deep charcoal, signature red.", background: "#141414", surface: "#202023", accent: "#e50914", text: "#f5f5f1" },
  { id: "midnight", name: "Midnight", description: "Cool navy with a soft violet glow.", background: "#101421", surface: "#1b2235", accent: "#b19aff", text: "#f0f2ff" },
  { id: "forest", name: "Forest", description: "Quiet greens. A warmer kind of dark.", background: "#101a17", surface: "#1d2b24", accent: "#81d5ad", text: "#eef6f0" },
  { id: "light", name: "Daylight", description: "Clean, bright, and easy on the eyes.", background: "#f4f5f7", surface: "#ffffff", accent: "#bf1232", text: "#1e2531" },
  { id: "system", name: "System", description: "Follow your device’s light or dark mode.", background: "#e9ecf1", surface: "#202023", accent: "#e50914", text: "#1e2531" },
] as const;
export type Theme = typeof themes[number]["id"];
export function resolveTheme(theme: Theme, prefersDark: boolean): Exclude<Theme, "system"> {
  return theme === "system" ? prefersDark ? "cinema" : "light" : theme;
}
export function applyTheme(theme: Theme) {
  const resolved = resolveTheme(theme, window.matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.dataset.theme = resolved;
  document.documentElement.dataset.themePreference = theme;
  document.querySelector('meta[name="theme-color"]')?.setAttribute("content", themes.find((item) => item.id === resolved)!.background);
}

export interface Preferences {
  theme: Theme;
  compactCards: boolean;
  reduceMotion: boolean;
  hideUnavailable: boolean;
  personalizedRows: boolean;
  keyboardShortcuts: boolean;
  defaultVolume: number;
}

export const defaultPreferences: Preferences = {
  theme: "cinema",
  compactCards: false,
  reduceMotion: false,
  hideUnavailable: false,
  personalizedRows: true,
  keyboardShortcuts: true,
  defaultVolume: 80,
};

export function readPreferences(): Preferences {
  try {
    const data = JSON.parse(localStorage.getItem("lumen.preferences") || "{}");
    const result = { ...defaultPreferences };
    for (const key of Object.keys(defaultPreferences) as (keyof Preferences)[]) {
      if (key === "theme") {
        if (themes.some((theme) => theme.id === data.theme)) result.theme = data.theme;
      } else if (key === "defaultVolume") {
        if (typeof data[key] === "number" && Number.isFinite(data[key])) {
          result.defaultVolume = Math.max(0, Math.min(100, data[key]));
        }
      } else if (typeof data[key] === "boolean") {
        result[key] = data[key];
      }
    }
    return result;
  } catch {
    return { ...defaultPreferences };
  }
}

export function uniqueHistory(history: HistoryEntry[]): HistoryEntry[] {
  const latest = new Map<string, HistoryEntry>();
  for (const item of [...history].sort((a, b) => b.played_at - a.played_at || b.id - a.id)) {
    if (!latest.has(item.path)) latest.set(item.path, item);
  }
  return [...latest.values()];
}

export function filterChannels(channels: Channel[], query: string, group: string, hideUnavailable: boolean): Channel[] {
  const terms = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
  return channels.filter((channel) =>
    (!hideUnavailable || !channel.last_error) &&
    (group === "all" || (channel.group_title || "Other") === group) &&
    terms.every((term) => `${channel.name} ${channel.group_title || ""}`.toLocaleLowerCase().includes(term))
  );
}

// Transparent recommendations: favorites and recently watched categories, not invented popularity.
export function recommendChannels(channels: Channel[], history: HistoryEntry[], favorites: Set<number>): Channel[] {
  const byId = new Map(channels.map((channel) => [channel.id, channel]));
  const byUrl = new Map(channels.map((channel) => [channel.url, channel]));
  const watched = new Set<string>();
  const categories = new Map<string, number>();
  for (const entry of uniqueHistory(history).slice(0, 30)) {
    watched.add(entry.path);
    const channel = (entry.channel_id ? byId.get(entry.channel_id) : undefined) || byUrl.get(entry.path);
    if (channel?.group_title) categories.set(channel.group_title, (categories.get(channel.group_title) || 0) + 1);
  }
  return channels.filter((channel) => !channel.last_error).map((channel, index) => ({
    channel,
    index,
    score: (favorites.has(channel.id) ? 6 : 0) + (categories.get(channel.group_title || "") || 0) * 3 + (watched.has(channel.url) ? -2 : 1),
  })).sort((a, b) => b.score - a.score || a.index - b.index).map(({ channel }) => channel);
}

export type ContentKind = "movie" | "series" | "live";

// Classify a library entry by the only signal a playlist gives us: its group and name.
// Playlists have no type field, so keywords decide which shelf a title belongs on.
export function contentKind(channel: Channel): ContentKind {
  // Trust the real TMDB media type over playlist naming/group heuristics whenever enriched.
  if (channel.tmdb_type === "movie") return "movie";
  if (channel.tmdb_type === "tv") return "series";
  const text = `${channel.group_title || ""} ${channel.name}`.toLocaleLowerCase();
  if (/\b(series|show|shows|season|episode|episodes|sitcom|soap|anime)\b/.test(text)) return "series";
  if (/\b(movie|movies|film|films|cinema|feature|vod|demo|documentary|short)\b/.test(text)) return "movie";
  return "live";
}

export function artworkCategory(group?: string): string {
  const value = (group || "").toLowerCase();
  if (/sport|outdoor|travel/.test(value)) return "adventure";
  if (/science|education|space/.test(value)) return "science";
  if (/news/.test(value)) return "news";
  if (/music/.test(value)) return "music";
  if (/movie|cinema|entertainment|demo/.test(value)) return "cinema";
  return "documentary";
}

// ---------------------------------------------------------------------------
// Categories: Group, Genre and Year.
//
// TMDB supplies real genres and release years for the titles it can match (movies
// and series). It has no records for live broadcast channels, so those get a genre
// inferred from their IPTV group string and land in the "Year unknown" bucket —
// which keeps every title reachable through all three facets.
// ---------------------------------------------------------------------------

export type FacetKind = "group" | "genre" | "year";

/** The cleared state for any facet, and the bucket for a missing value. */
export const ALL_FACETS = "all";
export const UNKNOWN_FACET = "unknown";

export interface FacetValue {
  value: string;
  label: string;
  count: number;
}

export interface FacetSelection {
  group: string;
  genre: string;
  year: string;
}

export interface Facets {
  groups: FacetValue[];
  genres: FacetValue[];
  years: FacetValue[];
  /** True when at least one title in the library carries real TMDB genres. */
  hasTmdbGenres: boolean;
  /** Titles carrying a real TMDB record, for an honest "enriched" reading. */
  enriched: number;
}

export const EMPTY_FACETS: FacetSelection = { group: ALL_FACETS, genre: ALL_FACETS, year: ALL_FACETS };

export function facetCount(selection: FacetSelection): number {
  return [selection.group, selection.genre, selection.year].filter((value) => value !== ALL_FACETS).length;
}

/**
 * IPTV group strings often pack several categories into one field, e.g.
 * "Culture;Entertainment;Music;News". Keep each tag independently browseable;
 * TMDB genres remain authoritative for matched movies and series.
 */
const GENRE_KEYWORDS: [RegExp, string][] = [
  [/news/i, "News"],
  [/sport/i, "Sports"],
  [/documentary/i, "Documentary"],
  [/music/i, "Music"],
  [/movie|film|cinema/i, "Movie"],
  [/series|shows?|soap|sitcom/i, "Series"],
  [/kids?|child|family|cartoon|animat/i, "Family"],
  [/science|education|space|learn/i, "Science"],
  [/religio|faith|church|gospel/i, "Religious"],
  [/shop|teleshop/i, "Shopping"],
  [/travel|outdoor|nature/i, "Travel"],
  [/legislat|politic|government|parliament/i, "Politics"],
  [/comedy/i, "Comedy"],
  [/lifestyle|cooking|food|garden/i, "Lifestyle"],
  [/weather/i, "Weather"],
  [/business|finance|money|stock/i, "Business"],
  [/entertainment|general/i, "Entertainment"],
  [/culture|art/i, "Culture"],
  [/health|medical/i, "Health"],
  [/auto|motor|car/i, "Motors"],
  [/demo/i, "Demo"],
];

/** Split playlist group tags so each is a first-class Group facet. */
export function groupsOf(group?: string): string[] {
  const tags = (group || "").split(";").map((token) => token.trim()).filter(Boolean);
  return tags.length ? [...new Set(tags)] : ["Other"];
}

/** Human-readable group label. */
export function groupLabel(group?: string): string {
  return groupsOf(group).join(" · ");
}

/** Infer every known genre represented by IPTV group tags or the title. */
export function inferGenres(group?: string, name = ""): string[] {
  const tokens = groupsOf(group);
  const inferred = tokens.flatMap((token) => {
    const hit = GENRE_KEYWORDS.find(([pattern]) => pattern.test(token));
    // Preserve unfamiliar group names instead of dropping their titles from Genre.
    return [hit?.[1] || (token === "Other" ? undefined : token)];
  }).filter((value): value is string => Boolean(value));
  if (inferred.length) return [...new Set(inferred)];
  const fromName = GENRE_KEYWORDS.filter(([pattern]) => pattern.test(name)).map(([, genre]) => genre);
  return fromName.length ? [...new Set(fromName)] : ["Uncategorized"];
}

/** Backward-compatible single best inferred genre helper. */
export function inferGenre(group?: string, name = ""): string | undefined {
  return inferGenres(group, name)[0];
}

export interface ChannelGenres {
  names: string[];
  source: "tmdb" | "inferred";
}

export function genresOf(channel: Channel): ChannelGenres {
  const fromTmdb = (channel.genre_names || "").split(";").map((value) => value.trim()).filter(Boolean);
  if (fromTmdb.length) return { names: [...new Set(fromTmdb)], source: "tmdb" };
  return { names: inferGenres(channel.group_title, channel.name), source: "inferred" };
}

export function yearOf(channel: Channel): number | null {
  const year = channel.release_year;
  return typeof year === "number" && year > 0 ? year : null;
}

export function hasTmdbArtwork(channel: Channel): boolean {
  return Boolean(channel.poster_path || channel.backdrop_path);
}

export function matchesFacets(channel: Channel, selection: FacetSelection): boolean {
  if (selection.group !== ALL_FACETS && !groupsOf(channel.group_title).includes(selection.group)) return false;

  if (selection.genre !== ALL_FACETS) {
    const { names } = genresOf(channel);
    if (selection.genre === UNKNOWN_FACET) {
      if (names.length) return false;
    } else if (!names.includes(selection.genre)) {
      return false;
    }
  }

  if (selection.year !== ALL_FACETS) {
    const year = yearOf(channel);
    if (selection.year === UNKNOWN_FACET) {
      if (year !== null) return false;
    } else if (year === null || String(year) !== selection.year) {
      return false;
    }
  }

  return true;
}

function facetList(
  counts: Map<string, number>,
  total: number,
  label: (value: string) => string,
  compare?: (a: FacetValue, b: FacetValue) => number,
): FacetValue[] {
  const values = [...counts.entries()].map(([value, count]) => ({ value, label: label(value), count }));
  values.sort(compare || ((a, b) => b.count - a.count || a.label.localeCompare(b.label)));
  return [{ value: ALL_FACETS, label: "All", count: total }, ...values];
}

/**
 * Build all three facets, counting each dimension inside the *other* two selected
 * facets so a chip's number is what choosing it would actually show.
 */
export function buildFacets(channels: Channel[], selection: FacetSelection): Facets {
  const countsFor = (dimension: FacetKind, valuesOf: (channel: Channel) => string[]) => {
    const others: FacetSelection = { ...selection, [dimension]: ALL_FACETS };
    const counts = new Map<string, number>();
    let total = 0;
    for (const channel of channels) {
      if (!matchesFacets(channel, others)) continue;
      total += 1;
      for (const value of valuesOf(channel)) counts.set(value, (counts.get(value) || 0) + 1);
    }
    return { counts, total };
  };

  const groups = countsFor("group", (channel) => groupsOf(channel.group_title));
  const genres = countsFor("genre", (channel) => {
    const { names } = genresOf(channel);
    return names.length ? names : [UNKNOWN_FACET];
  });
  const years = countsFor("year", (channel) => {
    const year = yearOf(channel);
    return year === null ? [UNKNOWN_FACET] : [String(year)];
  });

  const labelGenre = (value: string) => (value === UNKNOWN_FACET ? "Genre unknown" : value);
  const labelYear = (value: string) => (value === UNKNOWN_FACET ? "Year unknown" : value);

  return {
    groups: facetList(groups.counts, groups.total, groupLabel),
    genres: facetList(genres.counts, genres.total, labelGenre),
    years: facetList(years.counts, years.total, labelYear, (a, b) => {
      if (a.value === UNKNOWN_FACET) return 1;
      if (b.value === UNKNOWN_FACET) return -1;
      return Number(b.value) - Number(a.value);
    }),
    hasTmdbGenres: channels.some((channel) => genresOf(channel).source === "tmdb"),
    enriched: channels.filter((channel) => typeof channel.tmdb_id === "number").length,
  };
}

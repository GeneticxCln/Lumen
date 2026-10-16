import type { Channel, HistoryEntry } from "./types.ts";
import { currentLocale, t } from "../i18n/index.ts";
import { episodeInfo } from "./episodes.ts";

/** A watched mark set by hand. It overrides what history says. */
export interface EpisodeMark {
  seriesKey: string;
  season: number;
  episode: number;
  watched: boolean;
  updatedAt: number;
}

/** What TMDB says about a followed series' schedule. */
export interface UpcomingSeries {
  tmdbId: number;
  name: string;
  status: string | null;
  nextAirDate: string | null;
  nextSeason: number | null;
  nextEpisode: number | null;
  nextName: string | null;
  lastAirDate: string | null;
  lastSeason: number | null;
  lastEpisode: number | null;
  /** A library entry of the series, so its card can open it. */
  channelId: number | null;
}

/** A finished episode is one played to this fraction (kept equal to the board's). */
const FINISHED_FRACTION = 0.92;

/**
 * The series an episode belongs to, as the key marks are stored under, or null when the title is
 * not an episode. A title matched on TMDB uses its TMDB id (stable across naming differences);
 * otherwise the normalised series name from the title.
 */
export function seriesIdOf(channel: Channel): string | null {
  const info = episodeInfo(channel);
  if (!info) return null;
  if (channel.tmdb_type === "tv" && channel.tmdb_id) return `tmdb:tv:${channel.tmdb_id}`;
  return `name:${info.seriesKey}`;
}

function markKey(seriesKey: string, season: number, episode: number): string {
  return `${seriesKey}|${season}|${episode}`;
}

/** Library entries whose history shows them played to the end. */
export function finishedChannelIds(history: HistoryEntry[], channels: Channel[]): Set<number> {
  const byId = new Map(channels.map((channel) => [channel.id, channel]));
  const byUrl = new Map(channels.map((channel) => [channel.url, channel]));
  const done = new Set<number>();
  for (const entry of history) {
    if (entry.duration <= 0 || entry.position / entry.duration < FINISHED_FRACTION) continue;
    const channel = (entry.channel_id !== undefined ? byId.get(entry.channel_id) : undefined) ?? byUrl.get(entry.path);
    if (channel) done.add(channel.id);
  }
  return done;
}

/** Watched means: a hand-set mark if there is one, otherwise played to the end. */
export function isEpisodeWatched(channel: Channel, marks: Map<string, EpisodeMark>, finished: Set<number>): boolean {
  const series = seriesIdOf(channel);
  const info = episodeInfo(channel);
  if (series && info) {
    const mark = marks.get(markKey(series, info.season, info.episode));
    if (mark) return mark.watched;
  }
  return finished.has(channel.id);
}

export function indexMarks(marks: EpisodeMark[]): Map<string, EpisodeMark> {
  return new Map(marks.map((mark) => [markKey(mark.seriesKey, mark.season, mark.episode), mark]));
}

export interface EpisodeRow {
  channel: Channel;
  season: number;
  episode: number;
  watched: boolean;
}

export interface Season {
  season: number;
  episodes: EpisodeRow[];
  watchedCount: number;
}

/**
 * The seasons of the series `current` belongs to, from the library. When several entries cover
 * the same episode (different sources), one is kept: a working one if there is one.
 */
export function buildSeasons(current: Channel, channels: Channel[], marks: EpisodeMark[], history: HistoryEntry[]): Season[] {
  const series = seriesIdOf(current);
  if (!series) return [];
  const index = indexMarks(marks);
  const finished = finishedChannelIds(history, channels);
  const picked = new Map<string, EpisodeRow>();
  for (const channel of channels) {
    if (seriesIdOf(channel) !== series) continue;
    const info = episodeInfo(channel)!;
    const key = `${info.season}|${info.episode}`;
    const held = picked.get(key);
    const row: EpisodeRow = { channel, season: info.season, episode: info.episode, watched: isEpisodeWatched(channel, index, finished) };
    // Keep the current entry when it is one of the candidates, then a working one, then the first.
    const better = !held || (channel.id === current.id) || (held.channel.last_error && !channel.last_error && held.channel.id !== current.id);
    if (better) picked.set(key, row);
  }
  const seasons = new Map<number, EpisodeRow[]>();
  for (const row of picked.values()) seasons.set(row.season, [...(seasons.get(row.season) ?? []), row]);
  return [...seasons.entries()]
    .sort(([a], [b]) => a - b)
    .map(([season, episodes]) => {
      episodes.sort((a, b) => a.episode - b.episode);
      return { season, episodes, watchedCount: episodes.filter((row) => row.watched).length };
    });
}

const DAY_MS = 86_400_000;

function daysBetween(from: string, to: string): number {
  return Math.round((Date.parse(`${to}T00:00:00Z`) - Date.parse(`${from}T00:00:00Z`)) / DAY_MS);
}

function code(season: number | null, episode: number | null): string {
  return season !== null && episode !== null ? `S${String(season).padStart(2, "0")}E${String(episode).padStart(2, "0")}` : "";
}

export interface UpcomingLabel {
  kind: "upcoming" | "aired";
  text: string;
}

/** "S03E04 airs tomorrow", "S03E03 aired 2 days ago". `today` is YYYY-MM-DD. */
export function describeUpcoming(item: UpcomingSeries, today: string): UpcomingLabel | null {
  if (item.nextAirDate && item.nextAirDate >= today) {
    const days = daysBetween(today, item.nextAirDate);
    const episode = code(item.nextSeason, item.nextEpisode);
    const text = days === 0 ? t("lib.upcoming.today", { code: episode })
      : days === 1 ? t("lib.upcoming.tomorrow", { code: episode })
      : days < 7 ? t("lib.upcoming.inDays", { code: episode, days })
      : t("lib.upcoming.on", { code: episode, date: new Date(`${item.nextAirDate}T00:00:00Z`).toLocaleDateString(currentLocale(), { month: "short", day: "numeric", timeZone: "UTC" }) });
    return { kind: "upcoming", text: text.trim() };
  }
  if (item.lastAirDate && item.lastAirDate <= today) {
    const days = daysBetween(item.lastAirDate, today);
    if (days > 14) return null;
    const episode = code(item.lastSeason, item.lastEpisode);
    const text = days === 0 ? t("lib.aired.today", { code: episode })
      : days === 1 ? t("lib.aired.yesterday", { code: episode })
      : t("lib.aired.daysAgo", { code: episode, days });
    return { kind: "aired", text: text.trim() };
  }
  return null;
}

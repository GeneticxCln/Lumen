import { t } from "../i18n/index.ts";
import type { Channel, HistoryEntry } from "./types.ts";
import { contentKind, uniqueHistory } from "./discovery.ts";
import { episodeInfo, nextEpisode } from "./episodes.ts";
import { fileLabel, fileNameOf, isLocalPath } from "./local.ts";
import { finishedChannelIds, indexMarks, isEpisodeWatched, seriesIdOf } from "./series.ts";
import type { EpisodeMark } from "./series.ts";

/** A title counts as finished once this much of it has played. */
export const COMPLETE_FRACTION = 0.92;
/** Shorter than this is a stray click, not something to resume. */
export const MIN_RESUME_SECONDS = 10;
export const ROW_LIMIT = 20;

export interface ContinueItem {
  key: string;
  title: string;
  /** 0 to 1. */
  progress: number;
  remainingSeconds: number;
  playedAt: number;
  /** Set for a library stream; absent for a file on this computer. */
  channel?: Channel;
  /** Set for a file on this computer. */
  path?: string;
}

export interface NextUpItem {
  channel: Channel;
  /** The episode you just finished. */
  after: string;
}

/** The Home board: what to pick up, what comes next, what is new, then the usual shelves. */
export interface Board {
  continueWatching: ContinueItem[];
  nextUp: NextUpItem[];
  fresh: Channel[];
  myList: Channel[];
  recommended: Channel[];
}

export interface BoardInput {
  /** The library as shown (sources only, unavailable titles already filtered as the user prefers). */
  channels: Channel[];
  /** The active profile's history. */
  history: HistoryEntry[];
  favorites: Set<number>;
  /** Library entries added since the last visit, newest first (from the backend). */
  newChannelIds: number[];
  /** Ranked recommendations; titles already on the board are removed from them. */
  recommended: Channel[];
  /** Watched marks set by hand; they override what history says. */
  marks?: EpisodeMark[];
}

function fraction(entry: HistoryEntry): number {
  return entry.duration > 0 ? entry.position / entry.duration : 0;
}

export function isInProgress(entry: HistoryEntry): boolean {
  return entry.duration > 0 && entry.position >= MIN_RESUME_SECONDS && fraction(entry) < COMPLETE_FRACTION;
}

export function isFinished(entry: HistoryEntry): boolean {
  return entry.duration > 0 && fraction(entry) >= COMPLETE_FRACTION;
}

export function buildBoard(input: BoardInput): Board {
  const byId = new Map(input.channels.map((channel) => [channel.id, channel]));
  const byUrl = new Map(input.channels.map((channel) => [channel.url, channel]));
  const recent = uniqueHistory(input.history);
  const locate = (entry: HistoryEntry): Channel | undefined =>
    (entry.channel_id !== undefined ? byId.get(entry.channel_id) : undefined) ?? byUrl.get(entry.path);

  const watchedChannelIds = new Set<number>();
  for (const entry of recent) {
    const channel = locate(entry);
    if (channel) watchedChannelIds.add(channel.id);
  }

  const shown = new Set<number>();
  const continueWatching: ContinueItem[] = [];
  // The most recent entry of a series decides whether it is being resumed.
  const seriesHandled = new Set<string>();
  const resumingSeries = new Set<string>();

  for (const entry of recent) {
    if (!isInProgress(entry)) {
      // Finished episodes still claim their series' "most recent" slot, so an older in-progress
      // episode of the same series is not resumed over a newer finished one.
      const finishedChannel = locate(entry);
      const finishedInfo = finishedChannel ? episodeInfo(finishedChannel) : null;
      if (finishedInfo) seriesHandled.add(finishedInfo.seriesKey);
      continue;
    }
    const channel = locate(entry);
    const info = channel ? episodeInfo(channel) : null;
    if (info && seriesHandled.has(info.seriesKey)) continue;
    if (info) seriesHandled.add(info.seriesKey);

    if (channel) {
      if (contentKind(channel) === "live") continue;
      continueWatching.push({
        key: `channel-${channel.id}`,
        title: channel.name,
        progress: fraction(entry),
        remainingSeconds: Math.max(0, entry.duration - entry.position),
        playedAt: entry.played_at,
        channel,
      });
      shown.add(channel.id);
      const series = seriesIdOf(channel);
      if (series) resumingSeries.add(series);
    } else if (isLocalPath(entry.path)) {
      continueWatching.push({
        key: `file-${entry.path}`,
        title: fileLabel(fileNameOf(entry.path)),
        progress: fraction(entry),
        remainingSeconds: Math.max(0, entry.duration - entry.position),
        playedAt: entry.played_at,
        path: entry.path,
      });
    }
  }

  // Next up: per series, the episode after the furthest one watched (by a mark or by finishing
  // it), unless the series is being resumed or that episode was already started.
  const marks = indexMarks(input.marks ?? []);
  const finished = finishedChannelIds(input.history, input.channels);
  const recency = new Map<string, number>();
  for (const entry of recent) {
    if (!isFinished(entry)) continue;
    const channel = locate(entry);
    const series = channel ? seriesIdOf(channel) : null;
    if (series) recency.set(series, Math.max(recency.get(series) ?? 0, entry.played_at * 1000));
  }
  for (const mark of input.marks ?? []) {
    if (mark.watched) recency.set(mark.seriesKey, Math.max(recency.get(mark.seriesKey) ?? 0, mark.updatedAt));
  }
  const furthest = new Map<string, { channel: Channel; season: number; episode: number }>();
  for (const channel of input.channels) {
    const series = seriesIdOf(channel);
    const info = episodeInfo(channel);
    if (!series || !info || !isEpisodeWatched(channel, marks, finished)) continue;
    const held = furthest.get(series);
    if (!held || info.season > held.season || (info.season === held.season && info.episode > held.episode)) {
      furthest.set(series, { channel, season: info.season, episode: info.episode });
    }
  }
  const candidates: { item: NextUpItem; recency: number }[] = [];
  for (const [series, last] of furthest) {
    if (resumingSeries.has(series)) continue;
    const following = nextEpisode(last.channel, input.channels);
    if (!following || watchedChannelIds.has(following.id) || isEpisodeWatched(following, marks, finished)) continue;
    candidates.push({ item: { channel: following, after: last.channel.name }, recency: recency.get(series) ?? 0 });
  }
  candidates.sort((a, b) => b.recency - a.recency);
  const nextUp = candidates.map((candidate) => candidate.item);
  for (const item of nextUp) shown.add(item.channel.id);

  const fresh: Channel[] = [];
  for (const id of input.newChannelIds) {
    const channel = byId.get(id);
    if (!channel || shown.has(id) || watchedChannelIds.has(id) || contentKind(channel) === "live") continue;
    fresh.push(channel);
    shown.add(id);
    if (fresh.length >= ROW_LIMIT) break;
  }

  return {
    continueWatching: continueWatching.slice(0, ROW_LIMIT),
    nextUp: nextUp.slice(0, ROW_LIMIT),
    fresh,
    myList: input.channels.filter((channel) => input.favorites.has(channel.id)).slice(0, ROW_LIMIT),
    recommended: input.recommended.filter((channel) => !shown.has(channel.id)).slice(0, ROW_LIMIT),
  };
}

/** "12 min left", "1 h 5 min left". */
export function remainingLabel(seconds: number): string {
  const minutes = Math.max(1, Math.round(seconds / 60));
  if (minutes < 60) return t("lib.remaining.minutes", { minutes });
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0 ? t("lib.remaining.hours", { hours }) : t("lib.remaining.hoursMinutes", { hours, minutes: rest });
}

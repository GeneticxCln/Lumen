import type { Channel, HistoryEntry } from "./types.ts";
import { contentKind, uniqueHistory } from "./discovery.ts";
import { episodeInfo, nextEpisode } from "./episodes.ts";
import { fileLabel, fileNameOf, isLocalPath } from "./local.ts";

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
  const nextUp: NextUpItem[] = [];
  // The most recent entry of a series decides what the series needs: resume it, or move on.
  const seriesHandled = new Set<string>();

  for (const entry of recent) {
    const channel = locate(entry);
    const info = channel ? episodeInfo(channel) : null;
    if (info && seriesHandled.has(info.seriesKey)) continue;
    if (info) seriesHandled.add(info.seriesKey);

    if (isInProgress(entry)) {
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
    } else if (isFinished(entry) && channel && info) {
      const following = nextEpisode(channel, input.channels);
      // Already started or watched: nothing to suggest.
      if (following && !watchedChannelIds.has(following.id)) {
        nextUp.push({ channel: following, after: channel.name });
        shown.add(following.id);
      }
    }
  }

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
  if (minutes < 60) return `${minutes} min left`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0 ? `${hours} h left` : `${hours} h ${rest} min left`;
}

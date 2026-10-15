import type { Channel } from "./types.ts";

export interface EpisodeInfo {
  /** Normalised series name taken from the text before the episode marker. */
  seriesKey: string;
  season: number;
  episode: number;
}

// Playlists have no episode fields, so the marker in the title is the only signal:
// "Show S02E05", "Show S2 E5" or "Show 2x05". \b keeps resolutions such as 1080x720 out.
const EPISODE_MARKERS = [/\bS(\d{1,2})\s*[ .]?E(\d{1,3})\b/i, /\b(\d{1,2})x(\d{1,3})\b/i];

/** Reads the series and episode numbers from a title, or returns null when it has no marker. */
export function episodeInfo(channel: Channel): EpisodeInfo | null {
  for (const marker of EPISODE_MARKERS) {
    const match = marker.exec(channel.name);
    if (!match) continue;
    const seriesKey = channel.name
      .slice(0, match.index)
      .replace(/[\s\-–:._]+$/u, "")
      .replace(/\s+/g, " ")
      .trim()
      .toLocaleLowerCase();
    // A marker with no series name in front of it cannot be matched to a series.
    if (!seriesKey) return null;
    return { seriesKey, season: Number(match[1]), episode: Number(match[2]) };
  }
  return null;
}

function isAfter(a: EpisodeInfo, b: EpisodeInfo): boolean {
  return a.season > b.season || (a.season === b.season && a.episode > b.episode);
}

/**
 * Returns the episode that comes straight after `current` in the same series, or null.
 * Titles that failed their last check are skipped, since they would not start playing.
 */
export function nextEpisode(current: Channel, channels: Channel[]): Channel | null {
  const now = episodeInfo(current);
  if (!now) return null;
  let best: { channel: Channel; info: EpisodeInfo } | null = null;
  for (const candidate of channels) {
    if (candidate.id === current.id || candidate.last_error) continue;
    const info = episodeInfo(candidate);
    if (!info || info.seriesKey !== now.seriesKey || !isAfter(info, now)) continue;
    // Keep the closest episode; on a tie the earlier library entry wins.
    if (!best || isAfter(best.info, info)) best = { channel: candidate, info };
  }
  return best?.channel ?? null;
}

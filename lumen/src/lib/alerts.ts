import type { UpcomingSeries } from "./series.ts";

/** An episode that has aired and that the user has not been told about yet. */
export interface EpisodeAlert {
  /** Stable identity, so each episode is announced once. */
  key: string;
  seriesName: string;
  season: number;
  episode: number;
  /** YYYY-MM-DD. */
  airDate: string;
}

/** How long after airing an episode still counts as new. */
export const NEW_WINDOW_DAYS = 3;

const DAY_MS = 86_400_000;

export function alertKey(tmdbId: number, season: number, episode: number): string {
  return `${tmdbId}:${season}:${episode}`;
}

/**
 * The episodes of followed series that aired in the last few days and are not in `notified`,
 * newest first. `today` is YYYY-MM-DD.
 */
export function newEpisodeAlerts(
  upcoming: UpcomingSeries[],
  today: string,
  notified: ReadonlySet<string>,
  windowDays = NEW_WINDOW_DAYS,
): EpisodeAlert[] {
  const alerts: EpisodeAlert[] = [];
  for (const item of upcoming) {
    const { lastAirDate, lastSeason, lastEpisode } = item;
    if (!lastAirDate || lastSeason === null || lastEpisode === null) continue;
    if (lastAirDate > today) continue;
    const age = Math.round((Date.parse(`${today}T00:00:00Z`) - Date.parse(`${lastAirDate}T00:00:00Z`)) / DAY_MS);
    if (!Number.isFinite(age) || age > windowDays) continue;
    const key = alertKey(item.tmdbId, lastSeason, lastEpisode);
    if (notified.has(key)) continue;
    alerts.push({ key, seriesName: item.name, season: lastSeason, episode: lastEpisode, airDate: lastAirDate });
  }
  return alerts.sort((a, b) => b.airDate.localeCompare(a.airDate) || a.seriesName.localeCompare(b.seriesName));
}

/** The remembered keys with `fresh` added, keeping only the most recent `limit`. */
export function rememberNotified(previous: readonly string[], fresh: readonly string[], limit = 300): string[] {
  const merged = [...previous.filter((key) => !fresh.includes(key)), ...fresh];
  return merged.slice(Math.max(0, merged.length - limit));
}

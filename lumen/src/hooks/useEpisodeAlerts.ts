import { useEffect } from "react";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { newEpisodeAlerts, rememberNotified } from "../lib/alerts";
import { upcomingEpisodes } from "../lib/tauri";
import { t, tn } from "../i18n/index.ts";

const ENABLED_KEY = "lumen.alerts.enabled";
const SEEN_KEY = "lumen.alerts.seen";
const CHANGED_EVENT = "lumen-alerts-changed";
const FIRST_CHECK_MS = 20_000;
const CHECK_EVERY_MS = 6 * 60 * 60 * 1000;
/** More than this at once become one summary, so a returning user is not flooded. */
const MAX_SEPARATE = 3;

function read(key: string): string | null {
  try { return localStorage.getItem(key); } catch { return null; }
}

function write(key: string, value: string | null): void {
  try {
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, value);
  } catch { /* storage can be unavailable; the setting then lasts until the page closes */ }
}

export function episodeAlertsEnabled(): boolean {
  return read(ENABLED_KEY) === "1";
}

/**
 * Turns the alerts on or off. Turning them on asks the system for permission to show
 * notifications and returns false if it was refused (the setting is then left off).
 */
export async function setEpisodeAlertsEnabled(on: boolean): Promise<boolean> {
  if (on) {
    const granted = (await isPermissionGranted()) || (await requestPermission()) === "granted";
    if (!granted) return false;
  }
  write(ENABLED_KEY, on ? "1" : null);
  window.dispatchEvent(new Event(CHANGED_EVENT));
  return true;
}

function seenFor(profileId: number | null): string[] {
  try {
    const parsed: unknown = JSON.parse(read(`${SEEN_KEY}.${profileId ?? 0}`) ?? "[]");
    return Array.isArray(parsed) ? parsed.filter((key): key is string => typeof key === "string") : [];
  } catch {
    return [];
  }
}

async function check(profileId: number | null): Promise<void> {
  if (!episodeAlertsEnabled()) return;
  const upcoming = (await upcomingEpisodes(true)) ?? [];
  const seen = seenFor(profileId);
  const today = new Date().toISOString().slice(0, 10);
  const alerts = newEpisodeAlerts(upcoming, today, new Set(seen));
  if (alerts.length === 0) return;

  const code = (season: number, episode: number) => `S${String(season).padStart(2, "0")}E${String(episode).padStart(2, "0")}`;
  const separate = alerts.length > MAX_SEPARATE ? alerts.slice(0, MAX_SEPARATE - 1) : alerts;
  for (const alert of separate) {
    sendNotification({
      title: t("alerts.newEpisode", { series: alert.seriesName }),
      body: t("alerts.episodeAired", { code: code(alert.season, alert.episode), date: alert.airDate }),
    });
  }
  if (alerts.length > separate.length) {
    const rest = alerts.slice(separate.length);
    sendNotification({
      title: tn("alerts.moreEpisodes", rest.length),
      body: rest.map((alert) => alert.seriesName).join(", "),
    });
  }
  write(`${SEEN_KEY}.${profileId ?? 0}`, JSON.stringify(rememberNotified(seen, alerts.map((alert) => alert.key))));
}

/**
 * Tells the user, with a desktop notification, when an episode of a series they follow has aired.
 * Opt-in (Settings); checks shortly after start and then every few hours while the app runs, and
 * each episode is announced once per profile.
 */
export function useEpisodeAlerts(profileId: number | null): void {
  useEffect(() => {
    const run = () => { check(profileId).catch(() => {}); };
    const first = window.setTimeout(run, FIRST_CHECK_MS);
    const every = window.setInterval(run, CHECK_EVERY_MS);
    window.addEventListener(CHANGED_EVENT, run);
    return () => {
      window.clearTimeout(first);
      window.clearInterval(every);
      window.removeEventListener(CHANGED_EVENT, run);
    };
  }, [profileId]);
}

import { t, tn } from "../i18n/index.ts";

/** Something the app was asked to open: from "Open with Lumen", a lumen:// link, or a dropped file. */
export type OpenTarget =
  | { kind: "media"; path: string }
  | { kind: "playlist"; path: string }
  | { kind: "subtitle"; path: string }
  | { kind: "stream"; url: string };

export interface LaunchPlan {
  /** The one thing to start playing: the first file, or else the first web address. */
  play: { kind: "media"; path: string } | { kind: "stream"; url: string } | null;
  /** Playlist files to add as sources. */
  playlists: string[];
  /** Subtitle files for what is playing. */
  subtitles: string[];
  /** Further media that were asked for but not played (only one thing plays at a time). */
  skipped: number;
}

export function planLaunch(targets: OpenTarget[]): LaunchPlan {
  const media = targets.filter((target): target is { kind: "media"; path: string } => target.kind === "media");
  const streams = targets.filter((target): target is { kind: "stream"; url: string } => target.kind === "stream");
  const play = media[0] ?? streams[0] ?? null;
  return {
    play,
    playlists: targets.flatMap((target) => (target.kind === "playlist" ? [target.path] : [])),
    subtitles: targets.flatMap((target) => (target.kind === "subtitle" ? [target.path] : [])),
    skipped: Math.max(0, media.length + streams.length - (play ? 1 : 0)),
  };
}

/** The sentence shown after opening, or null when there is nothing to add to what the user sees. */
export function launchNotice(plan: LaunchPlan, importedPlaylists: number, subtitlesWithoutVideo: boolean): string | null {
  const parts: string[] = [];
  if (importedPlaylists > 0) parts.push(tn("lib.launch.added", importedPlaylists));
  if (plan.skipped > 0) parts.push(tn("lib.launch.skipped", plan.skipped));
  if (subtitlesWithoutVideo) parts.push(t("lib.launch.subtitleFirst"));
  return parts.length ? parts.join(" ") : null;
}

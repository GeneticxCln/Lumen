// Helpers for the media that lives on this computer. Pure functions only, so the Discovery
// page stays a presentational component and these can be checked without a DOM.

export type MediaKind = "video" | "audio" | "playlist" | "other";

const VIDEO_EXT = ["mp4", "mkv", "avi", "mov", "webm", "ts", "m2ts"];
const AUDIO_EXT = ["mp3", "flac", "wav", "ogg", "opus", "m4a"];
const PLAYLIST_EXT = ["m3u", "m3u8"];

/** Folders people actually keep media in, so the interesting rows are scanned first. */
export const MEDIA_ROOT_HINT = /^(videos?|movies?|films?|downloads?|media|music|series|shows?|tv|documentaries|recordings?|captures?)$/i;

/** How many top-level folders and subfolders one scan reads. */
export const MAX_ROOTS = 6;
export const MAX_SUBDIRS = 8;

export function extensionOf(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
}

export function kindOf(name: string): MediaKind {
  const ext = extensionOf(name);
  if (VIDEO_EXT.includes(ext)) return "video";
  if (AUDIO_EXT.includes(ext)) return "audio";
  if (PLAYLIST_EXT.includes(ext)) return "playlist";
  return "other";
}

/** "The.Great_Film (2019).mkv" reads better as "The Great Film (2019)". */
export function fileLabel(name: string): string {
  const dot = name.lastIndexOf(".");
  const base = dot > 0 ? name.slice(0, dot) : name;
  return base.replace(/[._]+/g, " ").replace(/\s+/g, " ").trim() || name;
}

export function fileNameOf(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

export function formatClock(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const secs = total % 60;
  const pad = (value: number) => value.toString().padStart(2, "0");
  return hours > 0 ? `${hours}:${pad(minutes)}:${pad(secs)}` : `${minutes}:${pad(secs)}`;
}

export function formatSize(bytes?: number): string {
  if (bytes === undefined || bytes === null) return "";
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(0)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  return `${(mb / 1024).toFixed(2)} GB`;
}

/** A stream URL is remote; anything else is a path on this machine. */
export function isLocalPath(path: string): boolean {
  return Boolean(path) && !/^[a-z][a-z0-9+.-]*:\/\//i.test(path);
}

export function parentOf(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  const cut = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return cut > 0 ? trimmed.slice(0, cut) : "";
}

/** Bounded-concurrency map, so a scan never opens every folder at once. */
export async function mapLimit<T, R>(items: T[], limit: number, worker: (item: T) => Promise<R>): Promise<R[]> {
  const results: R[] = new Array(items.length);
  let cursor = 0;
  const runners = Array.from({ length: Math.max(1, Math.min(limit, items.length)) }, async () => {
    while (cursor < items.length) {
      const index = cursor++;
      results[index] = await worker(items[index]);
    }
  });
  await Promise.all(runners);
  return results;
}

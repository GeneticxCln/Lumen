// Tauri & HTTP Bridge API wrapper — zero fake demo data, talks to real SQLite/Rust/Sidecar
import { invoke } from "@tauri-apps/api/core";
import { listen, Event } from "@tauri-apps/api/event";
import type {
  Channel,
  Playlist,
  LocalFile,
  PlayerState,
  Settings,
  ExtractResponse,
  HarvestResponse,
  ProbeResponse,
  ProbeResult,
  HistoryEntry,
  Profile,
  TmdbStatus,
  EnrichResult,
  SearchProviderConfig,
  SearchResponse,
} from "./types";

export type {
  Channel,
  Playlist,
  LocalFile,
  PlayerState,
  Settings,
  ExtractResponse,
  HarvestResponse,
  ProbeResponse,
  ProbeResult,
  HistoryEntry,
  Profile,
  TmdbStatus,
  EnrichResult,
  SearchProviderConfig,
  SearchResponse,
};

/**
 * TMDB image CDN. The configured sizes are served by `tmdb_status` so the UI never
 * hard-codes a size the backend disagrees with.
 */
export const TMDB_IMAGE_BASE = "https://image.tmdb.org/t/p/";
/** Kept in step with the sizes the backend reports from `tmdb_status`. */
export const TMDB_POSTER_SIZE = "w342";
export const TMDB_BACKDROP_SIZE = "w780";

export function tmdbImage(path: string | undefined, size: string): string {
  return path ? `${TMDB_IMAGE_BASE}${size}${path}` : "";
}

export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

// Re-exported here so components have one import site; the implementation lives in ./errors
// so it can be regression-tested without loading the Tauri runtime.
export { errorMessage, errorText } from "./errors";

async function callBackend<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  if (isTauriRuntime()) {
    return invoke<T>(cmd, args);
  }
  const res = await fetch(`/api/invoke/${cmd}`, {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Lumen-Client": "local-preview" },
    body: JSON.stringify(args),
    signal: AbortSignal.timeout(60000),
  });
  if (!res.ok) {
    let errText = `${res.status} ${res.statusText}`;
    try {
      const data = await res.json();
      if (data?.detail) errText = data.detail;
    } catch {
      // ignore
    }
    throw new Error(errText);
  }
  return res.json() as Promise<T>;
}

export function resolvePlayableSrc(url: string): string {
  if (!url) return "";
  if (
    url.startsWith("http://") ||
    url.startsWith("https://") ||
    url.startsWith("blob:")
  ) {
    return url;
  }
  // Local filesystem path -> stream via backend file endpoint
  return `/api/local-media?path=${encodeURIComponent(url)}&client=local-preview`;
}

// Player commands
export async function playUrl(
  url: string,
  headers?: Record<string, string>,
  channelId?: number
) {
  return callBackend("play_url", { url, headers, channelId });
}

export async function pause() {
  return callBackend("pause");
}

export async function seek(seconds: number) {
  return callBackend("seek", { seconds });
}

export async function setVolume(vol: number) {
  return callBackend("set_volume", { vol });
}

export async function stop() {
  return callBackend("stop");
}

// Playlist commands
export async function importPlaylistFile(path: string) {
  return callBackend<number>("import_playlist_file", { path });
}

export async function importPlaylistUrl(url: string, name?: string) {
  return callBackend<number>("import_playlist_url", { url, name });
}

export async function deletePlaylist(playlistId: number) {
  return callBackend("delete_playlist_cmd", { playlistId });
}

export async function listPlaylists() {
  return callBackend<Playlist[]>("list_playlists_cmd");
}

export async function listChannels(playlistId: number, query?: string, group?: string) {
  return callBackend<Channel[]>("list_channels_cmd", {
    playlistId,
    query: query || null,
    group: group || null,
  });
}

// Search engine
export async function searchEngine(query: string, providers: SearchProviderConfig[] = [], signal?: AbortSignal) {
  if (isTauriRuntime()) return invoke<SearchResponse>("search_engine", { query, providers, checkAvailability: true });
  const response = await fetch("/api/invoke/search_engine", {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Lumen-Client": "local-preview" },
    body: JSON.stringify({ query, providers, checkAvailability: true }),
    signal: signal || AbortSignal.timeout(60000),
  });
  if (!response.ok) {
    const payload = await response.json().catch(() => null);
    throw new Error(payload?.detail || `${response.status} ${response.statusText}`);
  }
  return response.json() as Promise<SearchResponse>;
}

export async function getSearchProviders() {
  const raw = await callBackend<string>("get_setting_value", { key: "search_providers" });
  try {
    const providers: SearchProviderConfig[] = raw ? JSON.parse(raw) : [];
    return { providers: providers.map(({ token: _token, ...provider }) => provider) };
  } catch {
    return { providers: [] };
  }
}

export async function setSearchProviders(providers: SearchProviderConfig[]) {
  const saved = await callBackend("set_setting_cmd", { key: "search_providers", value: JSON.stringify(providers) });
  return { providers: providers.map(({ token: _token, ...provider }) => provider), saved };
}

export async function getSettingValue(key: string) {
  return callBackend<string | null>("get_setting_value", { key });
}

export async function setSettingValue(key: string, value: string) {
  return callBackend("set_setting_cmd", { key, value });
}

// Sidecar commands
export async function extractUrl(url: string) {
  return callBackend<ExtractResponse>("extract_url", { url });
}

export async function harvestPage(url: string) {
  return callBackend<HarvestResponse>("harvest_page", { url });
}

export async function probeUrls(ids: number[]) {
  return callBackend<ProbeResponse>("probe_urls", { ids });
}

// Profiles. Watch history and My List are scoped to the active profile; the library is shared.
export async function listProfiles() {
  return callBackend<Profile[]>("list_profiles_cmd");
}

/** The profile the app is acting as, including whether a PIN locks it. */
export async function activeProfile() {
  return callBackend<Profile>("active_profile_cmd");
}

export async function createProfile(name: string, color?: string) {
  return callBackend<Profile>("create_profile_cmd", { name, color });
}

export async function renameProfile(profileId: number, name: string) {
  return callBackend<Profile>("rename_profile_cmd", { profileId, name });
}

/** Returns the profiles that remain after the deletion. */
export async function deleteProfile(profileId: number) {
  return callBackend<Profile[]>("delete_profile_cmd", { profileId });
}

/** Sets a profile's PIN, or clears it when `pin` is undefined or empty. */
export async function setProfilePin(profileId: number, pin?: string) {
  return callBackend<Profile>("set_profile_pin_cmd", { profileId, pin: pin ? pin : null });
}

/**
 * Switch to a profile and unlock it. A profile with a PIN rejects activation without the
 * correct PIN, so this is both the switch and the unlock.
 */
export async function activateProfile(profileId: number, pin?: string) {
  return callBackend<Profile>("activate_profile_cmd", { profileId, pin: pin ? pin : null });
}

// Favorites & History
export async function toggleFavorite(channelId: number) {
  return callBackend<boolean>("toggle_favorite_cmd", { channelId });
}

export async function listFavorites() {
  return callBackend<number[]>("list_favorites_cmd");
}

export async function getHistory(limit?: number) {
  return callBackend<HistoryEntry[]>("get_history_cmd", { limit });
}

export async function saveProgress(
  path: string,
  position: number,
  duration: number,
  channelId?: number
) {
  return callBackend("save_progress_cmd", { channelId, path, position, duration });
}

// Local files
export async function listLocalDir(path: string) {
  return callBackend<LocalFile[]>("list_local_dir", { path });
}

/** Fetch a cached frame from a local video without ever exposing a filesystem URL to the page. */
const localThumbnailCache = new Map<string, Promise<string>>();
const MAX_LOCAL_THUMBNAIL_URLS = 48;

export function localThumbnail(path: string): Promise<string> {
  const cached = localThumbnailCache.get(path);
  if (cached) {
    localThumbnailCache.delete(path);
    localThumbnailCache.set(path, cached);
    return cached;
  }

  const request = (async () => {
    if (isTauriRuntime()) {
      const encoded = await invoke<string>("local_thumbnail", { path });
      return `data:image/jpeg;base64,${encoded}`;
    }
    const response = await fetch(`/api/local-thumbnail?path=${encodeURIComponent(path)}&client=local-preview`);
    if (!response.ok) throw new Error(`Thumbnail unavailable (${response.status})`);
    const bytes = new Uint8Array(await response.arrayBuffer());
    let binary = "";
    for (let offset = 0; offset < bytes.length; offset += 0x8000) {
      binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000));
    }
    return `data:image/jpeg;base64,${btoa(binary)}`;
  })();
  localThumbnailCache.set(path, request);
  request.then((url) => {
    while (localThumbnailCache.size > MAX_LOCAL_THUMBNAIL_URLS) {
      const oldest = localThumbnailCache.keys().next().value;
      if (oldest === undefined) break;
      const stale = localThumbnailCache.get(oldest);
      localThumbnailCache.delete(oldest);
      // The decoded data URL may still be referenced by a mounted card; dropping this cache
      // reference bounds memory without invalidating that image.
      void stale?.catch(() => {});
    }
    return url;
  }).catch(() => {
    if (localThumbnailCache.get(path) === request) localThumbnailCache.delete(path);
  });
  return request;
}

// Settings
export async function getSettings() {
  return callBackend<Settings>("get_settings");
}

export async function setSetting(key: string, value: string) {
  return callBackend("set_setting_cmd", { key, value });
}

// TMDB metadata enrichment. The API key is written through setSetting under the
// `tmdb_api_key` key so the backend has a single credential path.
export const TMDB_API_KEY_SETTING = "tmdb_api_key";

export async function tmdbStatus() {
  return callBackend<TmdbStatus>("tmdb_status");
}

export async function enrichTmdb(limit?: number, force?: boolean) {
  return callBackend<EnrichResult>("tmdb_enrich", { limit, force: force || null });
}

export async function clearTmdbEnrichment() {
  return callBackend<{ cleared: number }>("tmdb_clear_enrichment");
}

// Player state events
export function onPlayerState(callback: (state: PlayerState) => void) {
  if (!isTauriRuntime()) {
    // Embedded browser playback reports state directly through VideoStage.
    return Promise.resolve(() => {});
  }
  return listen<PlayerState>("player://state", (event: Event<PlayerState>) => {
    callback(event.payload);
  });
}

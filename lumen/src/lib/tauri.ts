// Tauri API wrapper — zero fake demo data, talks to real SQLite/Rust backend
import { invoke } from "@tauri-apps/api/core";
import { listen, Event } from "@tauri-apps/api/event";
import type {
  Channel,
  Playlist,
  LocalFile,
  PlayerState,
  PlayerTrackKind,
  Settings,
  ExtractResponse,
  ExtractorStatus,
  HarvestResponse,
  ProbeResponse,
  ProbeResult,
  HistoryEntry,
  Profile,
  TmdbStatus,
  EnrichResult,
  SearchProviderConfig,
  SearchResponse,
  SyncState,
  SyncStatus,
  PlayerPrefs,
  PlayingItem,
  SubtitleCandidate,
  SubtitleSearch,
  SubtitleState,
  BrowseTitle,
  CastDevice,
  CastState,
  UpdateAvailable,
  FoundLinks,
  LinkCheck,
  BulkAdded,
  UpdateMode,
  UpdateState,
} from "./types";
import type { PosterCandidate } from "./types";
import type { EpisodeMark, UpcomingSeries } from "./series";
import type { OpenTarget } from "./launch";

export type {
  Channel,
  Playlist,
  LocalFile,
  PlayerState,
  PlayerTrackKind,
  Settings,
  ExtractResponse,
  ExtractorStatus,
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

// Re-exported here so components have one import site; the implementation lives in ./errors
// so it can be regression-tested without loading the Tauri runtime.
export { errorMessage, errorText } from "./errors";

// Every backend call is a Tauri command. There is no browser/HTTP fallback.
function callBackend<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  return invoke<T>(cmd, args);
}

// Player commands
export async function playUrl(
  url: string,
  headers?: Record<string, string>,
  channelId?: number,
  volume?: number,
  audioUrl?: string
) {
  return callBackend("play_url", { url, headers, channelId, volume, audioUrl });
}

export interface FallbackSource {
  url: string;
  headers?: Record<string, string> | null;
  channel_id?: number | null;
}

// The next ranked source for the title that failed to play, or null when there is none.
export async function nextFallbackSource(failedUrl: string): Promise<FallbackSource | null> {
  return callBackend("next_fallback_source", { failedUrl });
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

export async function setPlayerFullscreen(enabled: boolean) {
  return callBackend("set_fullscreen", { enabled });
}

export async function setPlayerTrack(kind: PlayerTrackKind, id: string) {
  return callBackend("set_player_track", { kind, id });
}

export async function setPlayerQuality(id: string) {
  return callBackend("set_player_quality", { id });
}

export interface EmbeddedSupport {
  supported: boolean;
  /** Why not, in words for the settings page. */
  reason: string | null;
}

/** Whether video can play inside the window (X11 only for now). */
export async function embeddedPlayerSupport() {
  return callBackend<EmbeddedSupport>("embedded_player_support");
}

/** Gives the embedded video `height` pixels above the page, or hides it. Returns whether it is showing in the window. */
export async function setEmbeddedVideo(visible: boolean, height: number) {
  return callBackend<boolean>("set_embedded_video", { visible, height });
}

export async function getPlayerPrefs() {
  return callBackend<PlayerPrefs>("get_player_prefs_cmd");
}

/** Saves the active profile's audio and subtitle preferences and applies them to the running player. */
export async function setPlayerPrefs(prefs: PlayerPrefs) {
  return callBackend<PlayerPrefs>("set_player_prefs_cmd", { prefs });
}

/** Shifts the current file's subtitles by `seconds` (positive is later). Returns the applied delay. */
export async function setSubtitleDelay(seconds: number) {
  return callBackend<number>("set_subtitle_delay_cmd", { seconds });
}

/** Sets the playback speed of the current file. Returns the speed that was applied. */
export async function setSpeed(speed: number) {
  return callBackend<number>("set_speed_cmd", { speed });
}

export async function addSubtitleFile(path: string) {
  return callBackend("add_subtitle_file_cmd", { path });
}

/** Whether online subtitle search is on, and which services are switched on. */
export async function subtitlesState() {
  return callBackend<SubtitleState>("subtitles_state_cmd");
}

export async function subtitlesSetOnline(on: boolean) {
  return callBackend("subtitles_set_online_cmd", { on });
}

export async function subtitlesSetService(id: string, enabled: boolean) {
  return callBackend("subtitles_set_service_cmd", { id, enabled });
}

/** Looks for subtitles for what is playing. Fails until online search has been turned on. */
export async function subtitlesSearch(item: PlayingItem, languages: string[]) {
  return callBackend<SubtitleSearch>("subtitles_search_cmd", { item, languages });
}

/** Downloads the chosen subtitle, remembers it for the title and shows it in the player. */
export async function subtitlesApply(item: PlayingItem, candidate: SubtitleCandidate) {
  return callBackend("subtitles_apply_cmd", { item, candidate });
}

export async function updateState() {
  return callBackend<UpdateState>("update_state_cmd");
}

export async function updateSetMode(mode: UpdateMode) {
  return callBackend("update_set_mode_cmd", { mode });
}

/** Looks for a newer release now; null when this is the newest. */
export async function updateCheck() {
  return callBackend<UpdateAvailable | null>("update_check_cmd");
}

/** Downloads, checks and installs the newest release; returns its tag. */
export async function updateInstall() {
  return callBackend<string>("update_install_cmd");
}

/** Starts the installed version in place of this one. */
export async function updateRestart() {
  return callBackend("update_restart_cmd");
}

/** Looks for TVs and speakers on the network (a few seconds). */
export async function castDiscover() {
  return callBackend<CastDevice[]>("cast_discover_cmd");
}

/** Sends what is playing to a device found by the last search. */
export async function castStart(rendererId: string, item: PlayingItem, startAt: number) {
  return callBackend<CastState>("cast_start_cmd", { rendererId, item, startAt });
}

export async function castControl(action: "play" | "pause" | "stop" | "seek", position?: number) {
  return callBackend("cast_control_cmd", { action, position });
}

/** The cast in progress, or null. */
export async function castState() {
  return callBackend<CastState | null>("cast_state_cmd");
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
export async function searchEngine(query: string, preferredLanguage?: string) {
  const args = { query, checkAvailability: true, ...(preferredLanguage ? { preferredLanguage } : {}) };
  return invoke<SearchResponse>("search_engine", args);
}

export async function getSearchProviders() {
  const providers = await callBackend<Omit<SearchProviderConfig, "token">[]>("get_search_providers");
  return { providers };
}

export async function setSearchProviders(providers: SearchProviderConfig[]) {
  const saved = await callBackend<Omit<SearchProviderConfig, "token">[]>("set_search_providers", { providers });
  return { providers: saved };
}

// Extraction, harvest & probe commands
export async function extractUrl(url: string) {
  return callBackend<ExtractResponse>("extract_url", { url });
}

export async function extractorStatus() {
  return callBackend<ExtractorStatus>("extractor_status_cmd");
}

export async function updateExtractor() {
  return callBackend<ExtractorStatus>("update_extractor_cmd");
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

/** `currentPin` is required when the profile has a PIN. */
export async function renameProfile(profileId: number, name: string, currentPin?: string) {
  return callBackend<Profile>("rename_profile_cmd", { profileId, name, currentPin: currentPin || null });
}

/** Returns the profiles that remain after the deletion. */
export async function deleteProfile(profileId: number, currentPin?: string) {
  return callBackend<Profile[]>("delete_profile_cmd", { profileId, currentPin: currentPin || null });
}

/**
 * Sets a profile's PIN, or clears it when `pin` is undefined or empty. Changing or clearing an
 * existing PIN requires that PIN as `currentPin`.
 */
export async function setProfilePin(profileId: number, pin?: string, currentPin?: string) {
  return callBackend<Profile>("set_profile_pin_cmd", { profileId, pin: pin ? pin : null, currentPin: currentPin || null });
}

/**
 * Switch to a profile and unlock it. A profile with a PIN rejects activation without the
 * correct PIN, so this is both the switch and the unlock.
 */
export async function activateProfile(profileId: number, pin?: string) {
  return callBackend<Profile>("activate_profile_cmd", { profileId, pin: pin ? pin : null });
}

/** Things the app was asked to open before the page was ready. Call once, when listening for `launch://open`. */
export async function takeLaunchTargets() {
  return callBackend<OpenTarget[]>("take_launch_targets");
}

/** Sorts dropped paths into media, playlists and subtitles (same rules as a launch). */
export async function classifyOpenTargets(paths: string[]) {
  return callBackend<OpenTarget[]>("classify_open_targets", { paths });
}

/** Watched marks the active profile set by hand. */
export async function listEpisodeMarks() {
  return callBackend<EpisodeMark[]>("list_episode_marks_cmd");
}

/** Marks episodes of one season watched or unwatched. The mark overrides what history says. */
export async function setEpisodesWatched(seriesKey: string, season: number, episodes: number[], watched: boolean) {
  return callBackend("set_episodes_watched_cmd", { seriesKey, season, episodes, watched });
}

/** Followed series with an episode still to air or one that just aired. `refresh` re-reads TMDB first. */
export async function upcomingEpisodes(refresh = false) {
  return callBackend<UpcomingSeries[]>("upcoming_episodes_cmd", { refresh });
}

/** Library entries added since this profile last visited. Empty on the first visit. */
export async function boardNewChannelIds() {
  return callBackend<number[]>("board_new_channel_ids");
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
  channelId?: number,
  // True for the final save when playback stops: it syncs at once instead of waiting for the
  // spacing applied to the periodic saves during playback.
  flush = false
) {
  return callBackend("save_progress_cmd", { channelId, path, position, duration, flush });
}

// Local files
export async function listLocalDir(path: string) {
  return callBackend<LocalFile[]>("list_local_dir", { path });
}

// Fetch cached local-video images without ever exposing filesystem URLs to the page.
const localThumbnailCache = new Map<string, Promise<string>>();
const MAX_LOCAL_THUMBNAIL_URLS = 48;

export function localFilmstrip(path: string): Promise<string> {
  return localImage(path, "filmstrip");
}

export function localThumbnail(path: string): Promise<string> {
  return localImage(path, "thumbnail");
}

// Renders the Settings frame choices for a video so the user can pick one. Missing frames come back as null.
export async function localPosterCandidates(path: string): Promise<PosterCandidate[]> {
  const candidates = await callBackend<PosterCandidate[]>("local_poster_candidates", { path });
  return candidates.map((candidate) => ({
    seconds: candidate.seconds,
    image: candidate.image ? `data:image/jpeg;base64,${candidate.image}` : null,
  }));
}

// The frame one file uses, or null when it follows the Settings frame.
export async function getLocalPosterFrame(path: string): Promise<number | null> {
  return callBackend<number | null>("get_local_poster_frame", { path });
}

// Sets one file's frame, or clears it with null so the file follows the Settings frame again.
export async function setLocalPosterFrame(path: string, seconds: number | null): Promise<void> {
  await callBackend<void>("set_local_poster_frame", { path, seconds });
}

// Posters depend on the saved frame time, so a change must drop the posters already held in memory.
export function clearLocalThumbnailCache() {
  for (const key of [...localThumbnailCache.keys()]) {
    if (key.startsWith("thumbnail:")) localThumbnailCache.delete(key);
  }
}

// Drops one file's cached poster after its frame changes, leaving other posters cached.
export function clearLocalPoster(path: string) {
  localThumbnailCache.delete(`thumbnail:${path}`);
}

function localImage(path: string, kind: "thumbnail" | "filmstrip"): Promise<string> {
  const cacheKey = `${kind}:${path}`;
  const command = kind === "filmstrip" ? "local_filmstrip" : "local_thumbnail";
  const cached = localThumbnailCache.get(cacheKey);
  if (cached) {
    localThumbnailCache.delete(cacheKey);
    localThumbnailCache.set(cacheKey, cached);
    return cached;
  }

  const request = (async () => {
    const encoded = await invoke<string>(command, { path });
    return `data:image/jpeg;base64,${encoded}`;
  })();
  localThumbnailCache.set(cacheKey, request);
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
    if (localThumbnailCache.get(cacheKey) === request) localThumbnailCache.delete(cacheKey);
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

// TMDB metadata enrichment. The API key goes to the OS keychain through its own command. It is never written to the settings table.
export async function setTmdbApiKey(key: string) {
  return callBackend("set_tmdb_api_key", { key });
}

// Cross-device sync. There are no accounts: a random sync key, kept in the OS keychain by the
// backend, encrypts everything before it leaves this device.
export async function syncState() {
  return callBackend<SyncState>("sync_state_cmd");
}

/** Saves the sync server address (empty restores the built-in one). Refused while sync is on. */
export async function syncSetServer(url: string) {
  return callBackend<SyncState>("sync_set_server", { url });
}

/** Turns sync on with a new key and returns the key text. Show it once and ask the user to keep it. */
export async function syncEnable() {
  return callBackend<string>("sync_enable");
}

/** Turns sync on with the key from another device. */
export async function syncJoin(key: string) {
  return callBackend<SyncState>("sync_join", { key });
}

export async function syncShowKey() {
  return callBackend<string>("sync_show_key");
}

/** Turns sync off on this device; with `deleteRemote` the synced copy is erased from the server too. */
export async function syncDisable(deleteRemote: boolean) {
  return callBackend<SyncState>("sync_disable", { deleteRemote });
}

export async function syncStatus() {
  return callBackend<SyncStatus>("sync_status_cmd");
}

export async function syncNow() {
  return callBackend<SyncStatus>("sync_now_cmd");
}

export async function tmdbStatus() {
  return callBackend<TmdbStatus>("tmdb_status");
}

export async function enrichTmdb(limit?: number, force?: boolean) {
  return callBackend<EnrichResult>("tmdb_enrich", { limit, force: force || null });
}

/** Trending and top-rated TMDB titles for Discovery. Fails when no key is configured. */
export async function browseTmdb() {
  return callBackend<{ titles: BrowseTitle[] }>("tmdb_browse");
}

export async function clearTmdbEnrichment() {
  return callBackend<{ cleared: number }>("tmdb_clear_enrichment");
}

/** The links in a text file, without the ones that are already sources. */
export async function bulkReadFile(path: string) {
  return callBackend<FoundLinks>("bulk_read_file_cmd", { path });
}

/** The links in pasted text. */
export async function bulkExtractText(text: string) {
  return callBackend<FoundLinks>("bulk_extract_text_cmd", { text });
}

/** Tests the links; each result arrives through `onBulkResult`. Resolves with how many were tested. */
export async function bulkCheck(urls: string[]) {
  return callBackend<number>("bulk_check_cmd", { urls });
}

export async function bulkCancel() {
  return callBackend("bulk_cancel_cmd");
}

/** Adds each link as its own source. */
export async function bulkAdd(urls: string[]) {
  return callBackend<BulkAdded>("bulk_add_cmd", { urls });
}

export function onBulkResult(callback: (check: LinkCheck) => void) {
  return listen<LinkCheck>("bulk://result", (event: Event<LinkCheck>) => callback(event.payload));
}

// Player state events
export function onPlayerState(callback: (state: PlayerState) => void) {
  return listen<PlayerState>("player://state", (event: Event<PlayerState>) => {
    callback(event.payload);
  });
}

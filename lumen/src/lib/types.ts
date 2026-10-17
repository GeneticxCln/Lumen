/** Whether this device syncs, and with which server. The sync key itself is never part of this. */
export interface SyncState {
  enabled: boolean;
  serverUrl: string | null;
  /** True when `serverUrl` is the address built into this version of Lumen. */
  defaultServer: boolean;
}

/** Sync progress on this device. `pending` counts local changes the server has not accepted yet. */
export interface SyncStatus {
  pending: number;
  lastSyncedAt: number | null;
  /** Records from the last pull that failed verification and were ignored. */
  skipped: number;
}

/** Whether, and through what, Lumen's own network traffic is being routed. */
export interface NetworkPrivacyState {
  enabled: boolean;
  upstream: string;
  username: string;
  hasPassword: boolean;
  status: "off" | "active" | "unavailable";
  error?: string;
}

export interface Channel {
  id: number;
  playlist_id: number;
  name: string;
  url: string;
  group_title?: string;
  tvg_id?: string;
  tvg_logo?: string;
  extra_headers?: string;
  last_ok?: number;
  last_error?: string;
  /** From the playlist's #EXTINF duration: false for a fixed-length title, true for -1, absent if unknown. */
  is_live?: boolean;
  // TMDB enrichment. Absent until the title has been matched against TMDB, which
  // only ever succeeds for movies and series — live channels stay unmatched.
  tmdb_id?: number;
  tmdb_type?: "movie" | "tv";
  poster_path?: string;
  backdrop_path?: string;
  overview?: string;
  release_year?: number;
  rating?: number;
  genre_names?: string;
  enriched_at?: number;
}

/** Whether a TMDB credential is available, and where the backend found it. */
export interface TmdbStatus {
  configured: boolean;
  key_source: "env" | "keychain" | "none";
  enriched: number;
  total: number;
  image_base: string;
  poster_size: string;
  backdrop_size: string;
}

/**
 * A title from TMDB's trending and top-rated lists. It is not necessarily in the local library,
 * so it is shown for information only and never offered for playback.
 */
export interface BrowseTitle {
  tmdb_id: number;
  tmdb_type: "movie" | "tv";
  name: string;
  poster_path?: string | null;
  backdrop_path?: string | null;
  overview?: string | null;
  release_year?: number | null;
  rating?: number | null;
  genre_names?: string | null;
}

/** Result of one enrichment run. `remaining` counts titles never attempted yet. */
export interface EnrichResult {
  enriched: number;
  matched: number;
  skipped: number;
  remaining: number;
  requests: number;
}

export interface Playlist {
  id: number;
  name: string;
  source: string;
  origin?: string;
  created_at: number;
  updated_at: number;
}

export interface LocalFile {
  name: string;
  path: string;
  is_dir: boolean;
  size?: number;
}

export type PlayerTrackKind = "audio" | "subtitle";

export interface PlayerTrack {
  id: string;
  kind: PlayerTrackKind;
  label: string;
  language?: string;
  selected: boolean;
}

export interface PlayerQuality {
  id: string;
  label: string;
}

export interface PlayerOptions {
  tracks: PlayerTrack[];
  audioTrack: string;
  subtitleTrack: string;
  qualityLevels: PlayerQuality[];
  quality: string;
  qualityHelp: string;
}

export interface PlayerState extends PlayerOptions {
  paused: boolean;
  time: number;
  duration: number;
  buffering: boolean;
  title?: string;
  error?: string;
}

export const DEFAULT_PLAYER_OPTIONS: PlayerOptions = {
  tracks: [],
  audioTrack: "auto",
  subtitleTrack: "off",
  qualityLevels: [],
  quality: "auto",
  qualityHelp: "Quality options appear when the stream exposes them.",
};

export const DEFAULT_PLAYER_STATE: PlayerState = {
  ...DEFAULT_PLAYER_OPTIONS,
  paused: true,
  time: 0,
  duration: 0,
  buffering: false,
};

// One offered poster frame. `image` is a data URL, or null when the video ends before that frame.
export interface PosterCandidate {
  seconds: number;
  image: string | null;
}

export interface Settings {
  hwdec: string;
  extra_mpv_args: string;
  cache: string;
  poster_frame_seconds: string;
  search_preferred_language: string;
  search_hide_other_languages: string;
  allow_lan_sources: string;
  /** "true" plays video inside the window instead of a separate mpv window (X11 only). */
  embed_player?: string;
}

export interface ExtractResponse {
  title: string;
  /** The resolved stream, for display only. It expires, so it is never what gets saved or played later. */
  url: string;
  audio_url?: string;
  headers: Record<string, string>;
  /** The library entry saved for the page. Play it by id; the stream is resolved at play time. */
  channel_id: number;
  page_url: string;
}

export interface ExtractorStatus {
  available: boolean;
  source: "managed" | "system" | "none";
  version?: string | null;
  pinnedVersion: string;
  binaryPath?: string | null;
}

export interface HarvestResponse {
  urls: string[];
}

export interface ProbeResponse {
  results: ProbeResult[];
}

export interface ProbeResult {
  url: string;
  live: boolean;
  status?: number;
  content_type?: string;
}

export interface SearchProviderConfig {
  id: string;
  name: string;
  endpoint: string;
  token?: string;
  timeout?: number;
  results_path?: string;
  title_field?: string;
  url_field?: string;
  aliases_field?: string;
  year_field?: string;
  media_type_field?: string;
  quality_field?: string;
  language_field?: string;
  headers_field?: string;
}

export interface SearchResult {
  id: string;
  title: string;
  url: string;
  provider: string;
  providers: string[];
  aliases: string[];
  year?: number | null;
  media_type?: string | null;
  quality?: string | null;
  language?: string | null;
  headers?: Record<string, string>;
  channel_id?: number;
  playlist_id?: number;
  group_title?: string;
  available?: boolean;
  status?: number | null;
  content_type?: string | null;
  score: number;
}

export interface SearchProviderStatus {
  id: string;
  name: string;
  status: "ok" | "error" | "timeout";
  count: number;
  error?: string;
}

export interface SearchResponse {
  query: string;
  results: SearchResult[];
  providers: SearchProviderStatus[];
}

export interface HistoryEntry {
  id: number;
  channel_id?: number;
  path: string;
  position: number;
  duration: number;
  played_at: number;
}

/**
 * An on-device profile. Lumen has no online accounts: a profile is a local identity that
 * scopes watch history and My List. `has_pin` reports whether a PIN locks it — the PIN
 * itself is only ever held by the backend, as an Argon2id hash.
 */
export interface Profile {
  id: number;
  name: string;
  color?: string;
  has_pin: boolean;
  created_at: number;
  last_used_at: number;
}

/** Audio and subtitle preferences of the active profile. */
export interface PlayerPrefs {
  /** An ISO 639 code, or empty to leave the choice to the player. */
  audioLang: string;
  /** "auto", "off", or a language code. */
  subtitles: string;
  /** 1 is the player's default size. */
  subScale: number;
  /** 100 is the bottom of the picture, 0 the top. */
  subPos: number;
  /** `#RRGGBB`. */
  subColor: string;
  subOutlineColor: string;
  /** `#RRGGBB`, or empty for no box behind the text. */
  subBackColor: string;
  subBold: boolean;
  /** A font family name, or empty for the player's default. */
  subFont: string;
  /** Seconds the back and forward buttons and arrow keys skip. */
  seekStep: number;
  /** Seconds before the end to offer the next episode; 0 is off. */
  autoplayCountdown: number;
}

/** What is playing, for subtitle lookups. */
export interface PlayingItem {
  channelId?: number;
  /** The address or file path being played. */
  source: string;
  /** The title shown to the user. */
  name?: string;
}

export interface SubtitleCandidate {
  provider: string;
  id: string;
  /** ISO 639-1. */
  language: string;
  release: string;
  hearingImpaired: boolean;
  downloads: number | null;
  fps: number | null;
  /** The service says it matches this exact file. */
  hashMatch: boolean;
  score: number;
  url: string;
}

export interface SubtitleSearch {
  searched: { title: string; year: number | null; season: number | null; episode: number | null; languages: string[] };
  candidates: SubtitleCandidate[];
  problems: { provider: string; message: string }[];
  /** Codes: no_title, no_episode, no_id. */
  hints: string[];
}

export interface SubtitleState {
  /** The user has agreed to look subtitles up online. */
  online: boolean;
  services: { id: string; enabled: boolean }[];
}

/** A TV or speaker on the network that can be sent a video. */
export interface CastDevice {
  id: string;
  name: string;
  address: string;
}

/** What a device reports about the cast: a UPnP state name, and seconds. */
export interface CastStatus {
  state: string;
  position: number;
  duration: number;
}

/** The cast in progress. `status` is missing when the device did not answer. */
export interface CastState {
  device: string;
  title: string;
  status: CastStatus | null;
}

export type UpdateMode = "auto" | "notify" | "off";

export interface UpdateState {
  current: string;
  mode: UpdateMode;
  /** `appImage` can be replaced by Lumen; `managed` is updated by the system that installed it. */
  kind: "appImage" | "managed";
  /** Seconds since the epoch. */
  lastChecked: number | null;
  /** The tag of a release that was installed and waits for a restart. */
  installed: string | null;
}

export interface UpdatePackage {
  name: string;
  size: number;
  sha256: string;
}

/** A newer release. `package` is missing when there is nothing verified to install. */
export interface UpdateAvailable {
  tag: string;
  page: string;
  package: UpdatePackage | null;
}

/** What the `update://ready` and `update://available` events carry. */
export interface UpdateEventPayload {
  tag: string;
  page: string;
}

/** What testing one playlist link found. */
export type LinkOutcome =
  | { kind: "playlist"; name: string; channels: number; sampled: number; responding: number }
  | { kind: "stream"; name: string }
  | { kind: "failed"; reason: string };

export interface LinkCheck {
  url: string;
  outcome: LinkOutcome;
}

/** The links found in a text, minus the ones that are already sources. */
export interface FoundLinks {
  links: string[];
  already: number;
}

export interface BulkAdded {
  added: { url: string; playlist_id: number }[];
  failed: { url: string; reason: string }[];
}

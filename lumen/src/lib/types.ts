/** Cross-device account state. `sub` is the Freebuff user ID; `verified` is false when the server could not be reached. */
export interface AccountStatus {
  signedIn: boolean;
  sub: string | null;
  verified: boolean;
}

/** Sync state on this device. `pending` counts local changes the server has not accepted yet. */
export interface SyncStatus {
  pending: number;
  lastSyncedAt: number | null;
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
  key_source: "env" | "keychain" | "memory" | "none";
  enriched: number;
  total: number;
  image_base: string;
  poster_size: string;
  backdrop_size: string;
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
}

export interface ExtractResponse {
  title: string;
  url: string;
  headers: Record<string, string>;
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

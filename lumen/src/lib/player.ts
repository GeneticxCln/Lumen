import type { PlayerOptions, PlayerQuality, PlayerTrack } from "./types.ts";
import { DEFAULT_PLAYER_OPTIONS } from "./types.ts";

export interface HlsLevelDescriptor {
  width?: number;
  height?: number;
  bitrate?: number;
  averageBitrate?: number;
  name?: string;
}

export interface HlsTrackDescriptor {
  id: number;
  name?: string;
  lang?: string;
  language?: string;
  default?: boolean;
}

export function formatBitrate(bitsPerSecond: number | undefined): string {
  if (!bitsPerSecond || !Number.isFinite(bitsPerSecond) || bitsPerSecond <= 0) return "";
  if (bitsPerSecond >= 1_000_000) return `${Number((bitsPerSecond / 1_000_000).toFixed(bitsPerSecond >= 10_000_000 ? 0 : 1))} Mbps`;
  return `${Math.round(bitsPerSecond / 1_000)} kbps`;
}

export function mapHlsQualityLevels(levels: HlsLevelDescriptor[]): PlayerQuality[] {
  return levels.map((level, index) => {
    const resolution = level.height ? `${level.height}p` : level.width ? `${level.width}×?` : `Variant ${index + 1}`;
    const label = level.name?.trim() || resolution;
    const bitrate = formatBitrate(level.averageBitrate || level.bitrate);
    return { id: `hls-level:${index}`, label: bitrate ? `${label} · ${bitrate}` : label };
  });
}

export function mapHlsTracks(
  audio: HlsTrackDescriptor[],
  subtitles: HlsTrackDescriptor[],
  audioIndex: number,
  subtitleIndex: number,
  subtitleVisible: boolean,
  levels: HlsLevelDescriptor[],
  currentLevel: number,
): PlayerOptions {
  const audioTracks = audio.map((track) => mapTrack(track, "audio", audioIndex));
  const subtitleTracks = subtitles.map((track) => mapTrack(track, "subtitle", subtitleIndex));
  const qualityLevels = mapHlsQualityLevels(levels);
  return {
    tracks: [...audioTracks, ...subtitleTracks],
    audioTrack: audioIndex >= 0 ? `hls-audio:${audioIndex}` : "auto",
    subtitleTrack: !subtitleVisible || subtitleIndex < 0 ? "off" : `hls-subtitle:${subtitleIndex}`,
    qualityLevels: qualityLevels.length ? [{ id: "hls-auto", label: "Auto" }, ...qualityLevels] : [],
    quality: currentLevel < 0 ? "hls-auto" : `hls-level:${currentLevel}`,
    qualityHelp: qualityLevels.length ? "HLS manifest renditions · Auto adapts to your connection" : DEFAULT_PLAYER_OPTIONS.qualityHelp,
  };
}

export function mapNativeSubtitleTracks(tracks: ArrayLike<Pick<TextTrack, "label" | "language" | "kind" | "mode">>): PlayerTrack[] {
  const output: PlayerTrack[] = [];
  for (let index = 0; index < tracks.length; index += 1) {
    const track = tracks[index];
    if (!track || !["subtitles", "captions"].includes(track.kind)) continue;
    const label = track.label.trim() || track.language.trim() || `Subtitle ${output.length + 1}`;
    output.push({
      id: `native-subtitle:${index}`,
      kind: "subtitle",
      label,
      language: track.language || undefined,
      selected: track.mode === "showing",
    });
  }
  return output;
}

function mapTrack(track: HlsTrackDescriptor, kind: PlayerTrack["kind"], selectedIndex: number): PlayerTrack {
  const language = track.lang || track.language;
  const base = track.name?.trim() || language || `${kind === "audio" ? "Audio" : "Subtitle"} ${track.id + 1}`;
  return {
    id: `hls-${kind}:${track.id}`,
    kind,
    label: `${base}${track.default ? " · Default" : ""}`,
    language,
    selected: track.id === selectedIndex,
  };
}

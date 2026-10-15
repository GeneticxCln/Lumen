import { formatBitrate, mapHlsQualityLevels, mapHlsTracks, mapNativeSubtitleTracks } from "./player.ts";

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}

assert(formatBitrate(850_000) === "850 kbps", "bitrates under 1 Mbps should render in kbps");
assert(formatBitrate(4_500_000) === "4.5 Mbps", "bitrates between 1 and 10 Mbps keep one decimal");
assert(formatBitrate(12_000_000) === "12 Mbps", "bitrates of 10 Mbps and above round to whole Mbps");
assert(formatBitrate(0) === "" && formatBitrate(undefined) === "", "missing bitrates render as empty");

const levels = mapHlsQualityLevels([
  { height: 1080, averageBitrate: 5_000_000 },
  { height: 720, bitrate: 2_800_000 },
  {},
]);
assert(levels.length === 3, "every manifest rendition should be offered");
assert(levels[0].id === "hls-level:0" && levels[0].label === "1080p · 5 Mbps", "height and bitrate should label a rendition");
assert(levels[1].label === "720p · 2.8 Mbps", "bitrate falls back to the declared bitrate");
assert(levels[2].label === "Variant 3", "unnamed renditions without resolution get a stable fallback label");

const tracks = mapHlsTracks(
  [
    { id: 0, name: "English", lang: "en", default: true },
    { id: 1, language: "fr" },
  ],
  [{ id: 0, name: "English CC", lang: "en" }],
  1,
  0,
  true,
  [{ height: 720 }, { height: 1080 }],
  -1,
);
assert(tracks.tracks.length === 3, "audio and subtitle tracks should both be exposed");
assert(tracks.tracks.find((track) => track.id === "hls-audio:1")?.selected === true, "the active audio track should be marked selected");
assert(tracks.audioTrack === "hls-audio:1", "the selected audio track id should be reported");
assert(tracks.subtitleTrack === "hls-subtitle:0", "a visible subtitle track should be reported as selected");
assert(tracks.qualityLevels[0].id === "hls-auto", "Auto should be offered first when renditions exist");
assert(tracks.quality === "hls-auto", "quality should default to Auto when no level is pinned");

const hidden = mapHlsTracks([], [{ id: 0, name: "English" }], -1, 0, false, [], -1);
assert(hidden.subtitleTrack === "off", "hidden subtitles should report Off");
assert(hidden.qualityLevels.length === 0, "no manifest renditions means no quality options");
assert(hidden.audioTrack === "auto", "no explicit audio track should report auto");

const nativeTracks = mapNativeSubtitleTracks([
  { kind: "subtitles", label: "", language: "de", mode: "showing" },
  { kind: "metadata", label: "ignored", language: "", mode: "hidden" },
  { kind: "captions", label: "Closed captions", language: "en", mode: "disabled" },
] as unknown as TextTrack[]);
assert(nativeTracks.length === 2, "only subtitle and caption tracks should be listed");
assert(nativeTracks[0].label === "de" && nativeTracks[0].selected, "an unlabeled showing track should use its language");
assert(nativeTracks[1].selected === false, "disabled tracks should not be marked selected");

console.log("Player track mapping checks passed.");

/** Languages offered for the audio and subtitle preferences. Codes are ISO 639-1, which mpv matches against track tags. */
export const LANGUAGES: { code: string; label: string }[] = [
  { code: "en", label: "English" },
  { code: "es", label: "Spanish" },
  { code: "fr", label: "French" },
  { code: "de", label: "German" },
  { code: "it", label: "Italian" },
  { code: "pt", label: "Portuguese" },
  { code: "nl", label: "Dutch" },
  { code: "sv", label: "Swedish" },
  { code: "no", label: "Norwegian" },
  { code: "da", label: "Danish" },
  { code: "fi", label: "Finnish" },
  { code: "pl", label: "Polish" },
  { code: "cs", label: "Czech" },
  { code: "hu", label: "Hungarian" },
  { code: "ro", label: "Romanian" },
  { code: "el", label: "Greek" },
  { code: "tr", label: "Turkish" },
  { code: "ru", label: "Russian" },
  { code: "uk", label: "Ukrainian" },
  { code: "ar", label: "Arabic" },
  { code: "he", label: "Hebrew" },
  { code: "hi", label: "Hindi" },
  { code: "th", label: "Thai" },
  { code: "vi", label: "Vietnamese" },
  { code: "id", label: "Indonesian" },
  { code: "zh", label: "Chinese" },
  { code: "ja", label: "Japanese" },
  { code: "ko", label: "Korean" },
];

/** Choices for how far the skip buttons jump, and how early the next episode is offered (0 is off). */
export const SEEK_STEPS = [5, 10, 15, 30, 60];
export const AUTOPLAY_COUNTDOWNS = [0, 5, 10, 15, 20, 30];
/** Playback speeds offered in the player. */
export const SPEEDS = [0.5, 0.75, 1, 1.25, 1.5, 2];

/** Subtitle size and delay steps used by the player's controls. */
export const SUB_SCALE_STEP = 0.1;
export const SUB_DELAY_STEP = 0.5;
export const MIN_SUB_SCALE = 0.5;
export const MAX_SUB_SCALE = 3;

export function clampScale(scale: number): number {
  return Math.round(Math.min(MAX_SUB_SCALE, Math.max(MIN_SUB_SCALE, scale)) * 100) / 100;
}

/** "+0.5 s", "-1.0 s", "0 s". */
export function formatDelay(seconds: number): string {
  if (seconds === 0) return "0 s";
  return `${seconds > 0 ? "+" : "-"}${Math.abs(seconds).toFixed(1)} s`;
}

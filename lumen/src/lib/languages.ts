import { currentLocale, t } from "../i18n/index.ts";

// Codes are ISO 639-1, which mpv matches against track tags. The English names are the fallback.
const ENGLISH_NAMES: [string, string][] = [
  ["en", "English"],
  ["es", "Spanish"],
  ["fr", "French"],
  ["de", "German"],
  ["it", "Italian"],
  ["pt", "Portuguese"],
  ["nl", "Dutch"],
  ["sv", "Swedish"],
  ["no", "Norwegian"],
  ["da", "Danish"],
  ["fi", "Finnish"],
  ["pl", "Polish"],
  ["cs", "Czech"],
  ["hu", "Hungarian"],
  ["ro", "Romanian"],
  ["el", "Greek"],
  ["tr", "Turkish"],
  ["ru", "Russian"],
  ["uk", "Ukrainian"],
  ["ar", "Arabic"],
  ["he", "Hebrew"],
  ["hi", "Hindi"],
  ["th", "Thai"],
  ["vi", "Vietnamese"],
  ["id", "Indonesian"],
  ["zh", "Chinese"],
  ["ja", "Japanese"],
  ["ko", "Korean"],
];

/** A language's name in the language of the page, falling back to the English name. */
function languageName(code: string, english: string): string {
  const locale = currentLocale();
  if (locale === "en") return english;
  try {
    return new Intl.DisplayNames([locale], { type: "language" }).of(code) ?? english;
  } catch {
    return english;
  }
}

/** Languages offered for the audio and subtitle preferences; `label` follows the language of the page. */
export const LANGUAGES: { code: string; label: string }[] = ENGLISH_NAMES.map(([code, english]) => ({
  code,
  get label() {
    return languageName(code, english);
  },
}));

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
  if (seconds === 0) return t("lib.units.secondsDelay", { value: "0" });
  const sign = seconds > 0 ? "+" : "-";
  return t("lib.units.secondsDelay", { value: `${sign}${Math.abs(seconds).toFixed(1)}` });
}

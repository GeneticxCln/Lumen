import { availableLocales, setLocale } from "./index.ts";

const STORAGE_KEY = "lumen.locale";

/**
 * The language to use: the first of the browser's preferred languages that is available, as an
 * exact match or by its main part (`de-AT` finds `de`), otherwise English.
 */
export function detectLocale(available: string[], preferred: readonly string[]): string {
  const have = new Set(available.map((code) => code.toLowerCase()));
  for (const wanted of preferred) {
    const code = wanted.toLowerCase();
    if (have.has(code)) return code;
    const main = code.split("-")[0];
    if (have.has(main)) return main;
  }
  return "en";
}

/** The language the user chose in Settings, or `null` for "follow the system". */
export function chosenLocale(): string | null {
  try {
    return localStorage.getItem(STORAGE_KEY);
  } catch {
    return null;
  }
}

/** Starts the page in the chosen language, else the system's. Call once before drawing anything. */
export function initLocale(): void {
  const chosen = chosenLocale();
  const preferred = typeof navigator !== "undefined" ? (navigator.languages?.length ? navigator.languages : [navigator.language]) : [];
  setLocale(chosen && availableLocales().includes(chosen) ? chosen : detectLocale(availableLocales(), preferred));
}

/** Remembers the user's choice (`null` follows the system) and switches to it. The page redraws on reload. */
export function chooseLocale(code: string | null): void {
  try {
    if (code === null) localStorage.removeItem(STORAGE_KEY);
    else localStorage.setItem(STORAGE_KEY, code);
  } catch {
    // Storage can be unavailable; the choice then lasts until the page is closed.
  }
  initLocale();
}

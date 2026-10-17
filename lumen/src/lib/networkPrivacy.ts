// Whether network privacy is on, and the one place that decides how a remote image (a TMDB
// poster/backdrop, or a playlist's `tvg-logo`) gets loaded: directly, the fast and normal way,
// while it's off (the default, and every other browser's behavior); through the guarded backend
// client once it's on, since going direct there would defeat the point of turning it on at all.
import { useEffect, useState, useSyncExternalStore } from "react";
import { fetchRemoteImage, networkPrivacyState } from "./tauri";

let enabled = false;
const listeners = new Set<() => void>();

function notify() {
  for (const listener of listeners) listener();
}

/** For the Settings page, which already has the fresh state from its own load/save calls. */
export function setNetworkPrivacyEnabled(next: boolean): void {
  if (next === enabled) return;
  enabled = next;
  notify();
}

/** Re-reads whether network privacy is on from the backend. Call once at app startup. */
export async function refreshNetworkPrivacyEnabled(): Promise<void> {
  try {
    setNetworkPrivacyEnabled((await networkPrivacyState()).enabled);
  } catch {
    // A transient read failure here should not flip image-loading behavior; keep the last
    // known value (off, until the first successful read).
  }
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function getSnapshot(): boolean {
  return enabled;
}

/** Reactive to `setNetworkPrivacyEnabled`. */
export function useNetworkPrivacyEnabled(): boolean {
  return useSyncExternalStore(subscribe, getSnapshot);
}

/** Caps how many fetched images are kept in memory at once, so a long session browsing a very
 * large library with privacy on cannot grow this without bound. */
const IMAGE_CACHE_LIMIT = 300;
const imageCache = new Map<string, string>();

function cacheImage(url: string, dataUrl: string) {
  if (imageCache.size >= IMAGE_CACHE_LIMIT) {
    const oldest = imageCache.keys().next().value;
    if (oldest !== undefined) imageCache.delete(oldest);
  }
  imageCache.set(url, dataUrl);
}

export interface RemoteImageResult {
  /** What an `<img>` should load; `undefined` while there is nothing to show yet. */
  src: string | undefined;
  /** True once a network-privacy-routed fetch has settled on failure (as opposed to still being
   * pending), so a caller can tell "not ready yet" apart from "show your own fallback now". */
  failed: boolean;
}

const NOTHING_TO_SHOW: RemoteImageResult = { src: undefined, failed: false };

/**
 * Resolves a remote image URL to what an `<img>` should actually load. While privacy is off,
 * that is `url` itself, synchronously, with no extra work. Once it's on, the image is fetched
 * once through the backend and cached here.
 */
export function useRemoteImageSrc(url: string | undefined): RemoteImageResult {
  const privacyEnabled = useNetworkPrivacyEnabled();
  const [result, setResult] = useState<RemoteImageResult>(() => {
    if (!url) return NOTHING_TO_SHOW;
    if (!privacyEnabled) return { src: url, failed: false };
    const cached = imageCache.get(url);
    return cached ? { src: cached, failed: false } : NOTHING_TO_SHOW;
  });

  useEffect(() => {
    if (!url) {
      setResult(NOTHING_TO_SHOW);
      return;
    }
    if (!privacyEnabled) {
      setResult({ src: url, failed: false });
      return;
    }
    const cached = imageCache.get(url);
    if (cached) {
      setResult({ src: cached, failed: false });
      return;
    }
    let active = true;
    setResult(NOTHING_TO_SHOW);
    fetchRemoteImage(url)
      .then((dataUrl) => {
        cacheImage(url, dataUrl);
        if (active) setResult({ src: dataUrl, failed: false });
      })
      .catch(() => {
        if (active) setResult({ src: undefined, failed: true });
      });
    return () => {
      active = false;
    };
  }, [url, privacyEnabled]);

  return result;
}

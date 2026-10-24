// Whether network privacy is on, and the one place that decides how a remote image (a TMDB
// poster/backdrop, or a playlist's `tvg-logo`) gets loaded: directly, the fast and normal way,
// while it's off (the default, and every other browser's behavior); through the guarded backend
// client once it's on, since going direct there would defeat the point of turning it on at all.
import { useEffect, useState, useSyncExternalStore } from "react";
import { fetchRemoteImage, networkPrivacyState } from "./tauri";
import { resolveImageSrc, type RemoteImageResult } from "./remoteImage.ts";

export { resolveImageSrc, type RemoteImageResult };

// `null` means "not read yet" (app just started) rather than "off": defaulting to off here
// would load images directly, unguarded, for however long the startup read takes, even when the
// saved setting is actually on. `resolveImageSrc` below treats `null` the same fail-closed way
// the backend's own `Route::Pending` does - never going direct until the real value is known.
let enabled: boolean | null = null;
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
    // A transient read failure here should not flip image-loading behavior; leave it unresolved
    // (still `null`) rather than guessing "off", so images keep waiting rather than going direct.
  }
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function getSnapshot(): boolean | null {
  return enabled;
}

/** Reactive to `setNetworkPrivacyEnabled`. `null` until the first successful read. */
export function useNetworkPrivacyEnabled(): boolean | null {
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

/**
 * Resolves a remote image URL to what an `<img>` should actually load. While privacy is off,
 * that is `url` itself, synchronously, with no extra work. Once it's on - or still unknown, at
 * startup - the image is fetched once through the backend and cached here.
 */
export function useRemoteImageSrc(url: string | undefined): RemoteImageResult {
  const privacyEnabled = useNetworkPrivacyEnabled();
  const [result, setResult] = useState<RemoteImageResult>(() =>
    resolveImageSrc(url, privacyEnabled, url ? imageCache.get(url) : undefined)
  );

  useEffect(() => {
    const immediate = resolveImageSrc(url, privacyEnabled, url ? imageCache.get(url) : undefined);
    setResult(immediate);
    // Direct (privacy off) or already cached: nothing to fetch. Unknown privacy state
    // (`null`) also takes this branch via `immediate.src === undefined`, so nothing is fetched
    // until the real value arrives and this effect reruns.
    if (!url || immediate.src !== undefined || privacyEnabled !== true) {
      return;
    }
    let active = true;
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

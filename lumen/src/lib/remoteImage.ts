// The one decision that governs whether a remote image (a TMDB poster/backdrop, or a playlist's
// `tvg-logo`) is allowed to load directly or must go through the guarded backend fetch. Kept
// free of any Tauri import so it is plain, unit-testable logic - see networkPrivacy.ts, which
// wraps this with the actual React state and backend call.

export interface RemoteImageResult {
  /** What an `<img>` should load; `undefined` while there is nothing to show yet. */
  src: string | undefined;
  /** True once a network-privacy-routed fetch has settled on failure (as opposed to still being
   * pending), so a caller can tell "not ready yet" apart from "show your own fallback now". */
  failed: boolean;
}

const NOTHING_TO_SHOW: RemoteImageResult = { src: undefined, failed: false };

/**
 * What to show for `url` right now, given what is currently known. `privacyEnabled === null`
 * (not read yet) is deliberately handled the same as `true`: both mean "don't go direct", since
 * the whole point of this module is that nothing bypasses the guarded fetch while privacy might
 * be on - the fail-closed default that closes the startup window where an unguarded image load
 * could otherwise slip out before the saved setting is even read.
 */
export function resolveImageSrc(
  url: string | undefined,
  privacyEnabled: boolean | null,
  cached: string | undefined
): RemoteImageResult {
  if (!url) return NOTHING_TO_SHOW;
  if (privacyEnabled === false) return { src: url, failed: false };
  return cached ? { src: cached, failed: false } : NOTHING_TO_SHOW;
}

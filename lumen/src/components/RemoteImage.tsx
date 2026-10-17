import { useEffect } from "react";
import { useRemoteImageSrc } from "../lib/networkPrivacy";

interface RemoteImageProps {
  src: string;
  className?: string;
  alt: string;
  loading?: "lazy" | "eager";
  decoding?: "async" | "sync" | "auto";
  referrerPolicy?: "no-referrer" | "origin";
  onError?: () => void;
}

/**
 * Drop-in replacement for `<img>` for a remote (non-bundled) image: a TMDB poster/backdrop, or a
 * playlist's `tvg-logo`. See `lib/networkPrivacy.ts` for what it actually loads and why. Renders
 * nothing while there is nothing to show yet — a network-privacy-routed fetch still pending, or
 * one that has failed. `onError` fires the same way for that failure and for a normal image-load
 * failure, so a caller's existing fallback artwork (shown once `onError` has fired) covers both;
 * a caller with no fallback of its own can simply leave it out.
 */
export function RemoteImage({ src, onError, ...imgProps }: RemoteImageProps) {
  const { src: resolved, failed } = useRemoteImageSrc(src);
  useEffect(() => {
    if (failed) onError?.();
  }, [failed, onError]);
  if (!resolved) return null;
  return <img src={resolved} onError={() => onError?.()} {...imgProps} />;
}

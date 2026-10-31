// A same-source retry schedule for a playback error, and the heuristic that decides whether
// retrying the exact same URL is worth attempting at all (a dropped connection is; an unsupported
// codec never will be, no matter how many times it's retried).

const SCHEDULE_MS = [1000, 3000, 7000];

/** The delay before retry attempt `attempt` (1-based), capped at the schedule's last entry. */
export function backoffDelayMs(attempt: number): number {
  const index = Math.min(Math.max(attempt, 1), SCHEDULE_MS.length) - 1;
  return SCHEDULE_MS[index];
}

/** How many same-source retries are attempted before falling back to another source. */
export const MAX_RETRIES = SCHEDULE_MS.length;

// mpv/ffmpeg's own words for the network-ish failures a reconnect might actually fix, taken from
// this codebase's real PlayerError::Mpv messages (see mpv_ipc.rs) rather than guessed generically.
const TRANSIENT_PATTERNS = [
  "timeout",
  "timed out",
  "connection",
  "reset",
  "refused",
  "network",
  "unreachable",
  "eof",
  "stopped responding",
  // mpv's own words for a load that did not complete. A 404 also says "failed to open", so
  // this is bounded by MAX_RETRIES and then falls through to the next ranked source.
  "loading failed",
  "failed to open",
  "could not open",
];

/** Whether `message` (an already-unwrapped error string) looks like a transient failure worth
 * retrying the same URL for, as opposed to e.g. an unsupported codec or a 404. */
export function isTransientPlaybackError(message: string): boolean {
  const lower = message.toLowerCase();
  return TRANSIENT_PATTERNS.some((pattern) => lower.includes(pattern));
}

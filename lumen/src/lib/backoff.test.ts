import { backoffDelayMs, isTransientPlaybackError, MAX_RETRIES } from "./backoff.ts";

function assert(value: unknown, message: string): asserts value { if (!value) throw new Error(message); }

assert(backoffDelayMs(1) === 1000, "first retry waits 1s");
assert(backoffDelayMs(2) === 3000, "second retry waits 3s");
assert(backoffDelayMs(3) === 7000, "third retry waits 7s");
assert(backoffDelayMs(4) === 7000, "a retry past the schedule's end is capped at its last value");
assert(backoffDelayMs(0) === 1000, "attempt 0 is treated as the first attempt");
assert(MAX_RETRIES === 3, "the schedule's length is the retry budget");

// Real PlayerError::Mpv messages this codebase actually produces (see mpv_ipc.rs), as rendered by
// errorMessage() with its "mpv error: " prefix.
const transientMessages = [
  "mpv error: mpv stopped responding (its control connection closed)",
  "mpv error: mpv stopped responding (its control connection closed): some tail",
  "mpv error: mpv IPC command timed out",
  "HTTP error: connection reset by peer",
  "HTTP error: connection refused",
  "network is unreachable",
  "mpv error: loading failed",
  "mpv error: failed to open https://cdn.example/live.m3u8",
  "mpv error: could not open stream",
];
for (const message of transientMessages) {
  assert(isTransientPlaybackError(message), `"${message}" should be treated as transient`);
}

const permanentMessages = [
  "mpv error: mpv failed to load the requested media",
  "HTTP error: HTTP 404 when fetching playlist",
  "Parse error: unsupported codec",
  "Invalid argument: That PIN is not correct",
  // mpv itself failing to start is a process/startup problem, not a dropped stream — retrying
  // the identical URL right away would not help, so this is deliberately not "transient".
  "mpv error: mpv did not open its control socket within 10 seconds",
];
for (const message of permanentMessages) {
  assert(!isTransientPlaybackError(message), `"${message}" should not be treated as transient`);
}

assert(isTransientPlaybackError("NETWORK ERROR"), "the classifier is case-insensitive");

console.log("Backoff schedule and transient-error classifier checks passed.");

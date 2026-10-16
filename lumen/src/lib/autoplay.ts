/** What the player should show about the next episode, from where playback is now. */
export interface NextUp {
  /** Whole seconds left in the current episode. */
  secondsLeft: number;
  /** The episode has ended: start the next one now. */
  startNow: boolean;
}

export interface NextUpInput {
  time: number;
  duration: number;
  paused: boolean;
  /** Seconds before the end to offer the next episode; 0 turns the offer off. */
  countdown: number;
  hasNext: boolean;
  /** The user dismissed the offer for this file. */
  dismissed: boolean;
}

/** A file shorter than this is not an episode worth chaining (a trailer, a clip). */
const MIN_EPISODE_SECONDS = 120;
/** With keep-open, the player pauses within a moment of the end; this counts as "ended". */
const ENDED_WITHIN_SECONDS = 1;

/** `null` while there is nothing to offer. */
export function nextUp(input: NextUpInput): NextUp | null {
  const { time, duration, paused, countdown, hasNext, dismissed } = input;
  if (!hasNext || dismissed || countdown <= 0) return null;
  if (!Number.isFinite(duration) || duration < MIN_EPISODE_SECONDS) return null;
  const remaining = duration - time;
  if (remaining > countdown) return null;
  const ended = remaining <= ENDED_WITHIN_SECONDS;
  // A user who paused inside the last seconds is not at the end yet: wait for them.
  if (paused && !ended) return null;
  return { secondsLeft: Math.max(0, Math.ceil(remaining)), startNow: ended };
}

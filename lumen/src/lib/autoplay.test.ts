import assert from "node:assert/strict";
import { nextUp } from "./autoplay.ts";

const base = { time: 0, duration: 2400, paused: false, countdown: 10, hasNext: true, dismissed: false };

assert.equal(nextUp({ ...base, time: 1000 }), null, "nothing is offered mid-episode");
assert.deepEqual(nextUp({ ...base, time: 2395 }), { secondsLeft: 5, startNow: false }, "the offer counts down");
assert.deepEqual(nextUp({ ...base, time: 2390 }), { secondsLeft: 10, startNow: false }, "it starts at the countdown length");
assert.equal(nextUp({ ...base, time: 2389 }), null, "and not before");
assert.deepEqual(nextUp({ ...base, time: 2399.6, paused: true }), { secondsLeft: 1, startNow: true }, "the end, where mpv pauses, starts the next episode");
assert.deepEqual(nextUp({ ...base, time: 2400, paused: true }), { secondsLeft: 0, startNow: true });

assert.equal(nextUp({ ...base, time: 2395, paused: true }), null, "paused in the last seconds waits for the user");
assert.equal(nextUp({ ...base, time: 2395, hasNext: false }), null, "no next episode, no offer");
assert.equal(nextUp({ ...base, time: 2395, dismissed: true }), null, "dismissed for this file");
assert.equal(nextUp({ ...base, time: 2395, countdown: 0 }), null, "turned off");
assert.equal(nextUp({ ...base, time: 60, duration: 90 }), null, "short clips are not chained");
assert.equal(nextUp({ ...base, duration: 0 }), null, "live streams have no end");
assert.equal(nextUp({ ...base, time: 5, duration: Number.NaN }), null);

console.log("autoplay.test.ts: ok");

import assert from "node:assert/strict";
import { describeSync, skippedWarning } from "./sync.ts";

assert.equal(describeSync({ pending: 0, lastSyncedAt: null, skipped: 0 }), "Not synced yet.");
assert.match(describeSync({ pending: 1, lastSyncedAt: null, skipped: 0 }), /^Not synced yet\. 1 change waiting to upload\.$/);
assert.match(describeSync({ pending: 3, lastSyncedAt: Date.UTC(2026, 0, 2, 12), skipped: 0 }), /^Last synced .+\. 3 changes waiting to upload\.$/);
assert.doesNotMatch(describeSync({ pending: 0, lastSyncedAt: Date.UTC(2026, 0, 2, 12), skipped: 0 }), /waiting/);

assert.equal(skippedWarning({ pending: 0, lastSyncedAt: null, skipped: 0 }), null);
assert.match(skippedWarning({ pending: 0, lastSyncedAt: null, skipped: 1 })!, /^1 synced record could not be verified and was ignored/);
assert.match(skippedWarning({ pending: 0, lastSyncedAt: null, skipped: 4 })!, /^4 synced records could not be verified and were ignored/);

console.log("sync.test.ts: ok");

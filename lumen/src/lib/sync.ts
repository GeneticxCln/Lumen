import type { SyncStatus } from "./types";

/** One line about the last sync, for the settings page. */
export function describeSync(sync: SyncStatus): string {
  const last = sync.lastSyncedAt
    ? `Last synced ${new Date(sync.lastSyncedAt).toLocaleString()}.`
    : "Not synced yet.";
  if (sync.pending === 0) return last;
  const changes = `${sync.pending} change${sync.pending === 1 ? "" : "s"}`;
  return `${last} ${changes} waiting to upload.`;
}

/** A warning when the last sync ignored records it could not verify; null when there is nothing to say. */
export function skippedWarning(sync: SyncStatus): string | null {
  if (sync.skipped <= 0) return null;
  const records = `${sync.skipped} synced record${sync.skipped === 1 ? "" : "s"}`;
  return `${records} could not be verified and ${sync.skipped === 1 ? "was" : "were"} ignored. The sync server may have altered or damaged ${sync.skipped === 1 ? "it" : "them"}.`;
}

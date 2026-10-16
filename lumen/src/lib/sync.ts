import { t, tn } from "../i18n/index.ts";
import type { SyncStatus } from "./types.ts";

/** One line about the last sync, for the settings page. */
export function describeSync(sync: SyncStatus): string {
  const last = sync.lastSyncedAt
    ? t("sync.last", { when: new Date(sync.lastSyncedAt).toLocaleString() })
    : t("sync.never");
  if (sync.pending === 0) return last;
  return `${last} ${tn("sync.pending", sync.pending)}`;
}

/** A warning when the last sync ignored records it could not verify; null when there is nothing to say. */
export function skippedWarning(sync: SyncStatus): string | null {
  if (sync.skipped <= 0) return null;
  return tn("sync.skipped", sync.skipped);
}

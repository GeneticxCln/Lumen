import { useEffect, useState } from "react";
import {
  accountSignIn,
  accountSignOut,
  accountStatus,
  errorMessage,
  isTauriRuntime,
  syncNow,
  syncStatus,
} from "../lib/tauri";
import type { AccountStatus, SyncStatus } from "../lib/types";

const signedOut: AccountStatus = { signedIn: false, sub: null, verified: true };

function describeSync(sync: SyncStatus): string {
  const last = sync.lastSyncedAt
    ? `Last synced ${new Date(sync.lastSyncedAt).toLocaleString()}.`
    : "Not synced yet.";
  if (sync.pending === 0) return last;
  const changes = `${sync.pending} change${sync.pending === 1 ? "" : "s"}`;
  return `${last} ${changes} waiting to upload.`;
}

/** Sign-in for syncing profiles, favorites and progress across devices. */
export function AccountSignIn() {
  const [status, setStatus] = useState<AccountStatus | null>(null);
  const [sync, setSync] = useState<SyncStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!isTauriRuntime()) {
      setStatus(signedOut);
      return;
    }
    let active = true;
    accountStatus()
      .then(async (next) => {
        if (!active) return;
        setStatus(next);
        if (next.signedIn) setSync(await syncStatus());
      })
      .catch((failure) => { if (active) setError(errorMessage(failure)); });
    return () => { active = false; };
  }, []);

  const run = async (action: () => Promise<AccountStatus>) => {
    setBusy(true);
    setError(null);
    try {
      const next = await action();
      setStatus(next);
      // Signing in starts a sync in the background; signing out drops the sync state.
      setSync(next.signedIn ? await syncStatus() : null);
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setBusy(false);
    }
  };

  const syncAction = async () => {
    setBusy(true);
    setError(null);
    try {
      setSync(await syncNow());
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setBusy(false);
    }
  };

  const signInAvailable = isTauriRuntime();

  return (
    <div className="account-signin">
      <h3 className="settings-section-title">Lumen account</h3>
      {!status && !error && <p className="muted-note">Checking sign-in…</p>}
      {status?.signedIn && (
        <p>
          Signed in{status.verified ? "" : " (could not reach the account server to confirm)"}. Your profiles, favorites
          and progress sync across your devices.
        </p>
      )}
      {status && !status.signedIn && (
        <p>
          Sign in to keep your favorites and watch progress in step across devices. Your library stays on this
          computer.
        </p>
      )}
      {status?.signedIn && sync && <p className="muted-note">{describeSync(sync)}</p>}
      {error && <div className="dialog-error" role="alert">{error}</div>}
      <div className="settings-actions">
        {status?.signedIn ? (
          <>
            <button className="secondary-button" disabled={busy} onClick={() => void syncAction()}>
              {busy ? "Syncing…" : "Sync now"}
            </button>
            <button className="secondary-button" disabled={busy} onClick={() => void run(accountSignOut)}>
              Sign out
            </button>
          </>
        ) : (
          <button
            className="primary-button"
            disabled={busy || !signInAvailable}
            onClick={() => void run(accountSignIn)}
          >
            {busy ? "Waiting for your browser…" : "Sign in with Freebuff"}
          </button>
        )}
      </div>
    </div>
  );
}

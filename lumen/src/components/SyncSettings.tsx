import { useEffect, useState } from "react";
import {
  errorMessage,
  syncDisable,
  syncEnable,
  syncJoin,
  syncSetServer,
  syncShowKey,
  syncState,
  syncStatus,
  syncNow,
} from "../lib/tauri";
import { describeSync, skippedWarning } from "../lib/sync";
import type { SyncState, SyncStatus } from "../lib/types";

type Mode = "idle" | "join" | "confirm-off" | "confirm-delete";

/**
 * Cross-device sync without an account. Turning it on makes a random key; another device joins by
 * typing that key. Everything is encrypted on this device before it is uploaded, so the server
 * only ever holds data it cannot read, and there is nobody who can recover a lost key.
 */
export function SyncSettings() {
  const [state, setState] = useState<SyncState | null>(null);
  const [sync, setSync] = useState<SyncStatus | null>(null);
  const [shownKey, setShownKey] = useState<string | null>(null);
  const [keyIsNew, setKeyIsNew] = useState(false);
  const [copied, setCopied] = useState(false);
  const [mode, setMode] = useState<Mode>("idle");
  const [joinText, setJoinText] = useState("");
  const [serverText, setServerText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    syncState()
      .then(async (next) => {
        if (!active) return;
        setState(next);
        setServerText(next.defaultServer ? "" : next.serverUrl ?? "");
        if (next.enabled) setSync(await syncStatus());
      })
      .catch((failure) => { if (active) setError(errorMessage(failure)); });
    return () => { active = false; };
  }, []);

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await action();
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setBusy(false);
    }
  };

  const syncAfterEnable = async () => {
    try {
      setSync(await syncNow());
    } catch (failure) {
      setError(`Sync is on, but the first sync failed: ${errorMessage(failure)}`);
    }
  };

  const turnOn = () => run(async () => {
    const key = await syncEnable();
    setShownKey(key);
    setKeyIsNew(true);
    setCopied(false);
    setState(await syncState());
    await syncAfterEnable();
  });

  const join = () => run(async () => {
    setState(await syncJoin(joinText));
    setJoinText("");
    setMode("idle");
    await syncAfterEnable();
  });

  const showKey = () => run(async () => {
    setShownKey(await syncShowKey());
    setKeyIsNew(false);
    setCopied(false);
  });

  const syncAction = () => run(async () => { setSync(await syncNow()); });

  const turnOff = (deleteRemote: boolean) => run(async () => {
    setState(await syncDisable(deleteRemote));
    setSync(null);
    setShownKey(null);
    setMode("idle");
  });

  const saveServer = () => run(async () => {
    const next = await syncSetServer(serverText);
    setState(next);
    setServerText(next.defaultServer ? "" : next.serverUrl ?? "");
  });

  const copyKey = async () => {
    if (!shownKey) return;
    try {
      await navigator.clipboard.writeText(shownKey);
      setCopied(true);
    } catch {
      setError("Could not copy to the clipboard. Select the key and copy it by hand.");
    }
  };

  const warning = sync ? skippedWarning(sync) : null;

  return (
    <div className="sync-settings">
      <h3 className="settings-section-title">Sync across devices</h3>
      {!state && !error && <p className="muted-note">Checking sync…</p>}

      {state && !state.enabled && (
        <>
          <p>
            Keep your profiles, My List and watch progress in step on your other computers. There is no
            account: Lumen makes a secret key on this device, and you type that key into your other devices.
          </p>
          <p className="muted-note">
            Everything is encrypted on this device before it is uploaded, so the sync server only holds data it
            cannot read. Because there is no account, nobody can recover a lost key. If you lose the key and every
            device that has it, the synced copy is gone. Your library on this computer is never affected.
          </p>
          <details className="sync-server">
            <summary>Sync server{state.serverUrl ? `: ${state.serverUrl}` : " (not set)"}</summary>
            <p className="muted-note">
              {state.defaultServer && state.serverUrl
                ? "This is the server built into this version of Lumen. You can run your own and enter its address here."
                : "Enter the address of a Lumen sync server. It must start with https://."}
            </p>
            <label htmlFor="sync-server-url">Server address</label>
            <input
              id="sync-server-url"
              type="url"
              value={serverText}
              placeholder="https://sync.example.com"
              disabled={busy}
              onChange={(event) => setServerText(event.target.value)}
            />
            <div className="settings-actions">
              <button className="secondary-button" disabled={busy} onClick={() => void saveServer()}>
                Save server address
              </button>
            </div>
          </details>
        </>
      )}

      {state?.enabled && (
        <>
          <p>Sync is on. Your profiles, My List and watch progress follow you across the devices that have your key.</p>
          {sync && <p className="muted-note">{describeSync(sync)}</p>}
          {state.serverUrl && <p className="muted-note">Server: {state.serverUrl}</p>}
          {warning && <div className="dialog-error" role="alert">{warning}</div>}
        </>
      )}

      {shownKey && (
        <div className="sync-key-panel" role="region" aria-label="Your sync key">
          <p className="sync-key" data-testid="sync-key">{shownKey}</p>
          <p className="muted-note">
            {keyIsNew
              ? "Write this key down or save it in a password manager now. You need it to add another device, and it cannot be recovered if you lose it."
              : "Anyone with this key can read and change your synced data. Only enter it on your own devices."}
          </p>
          <div className="settings-actions">
            <button className="secondary-button" onClick={() => void copyKey()}>
              {copied ? "Copied" : "Copy key"}
            </button>
            <button className="secondary-button" onClick={() => setShownKey(null)}>
              {keyIsNew ? "I have saved it" : "Hide key"}
            </button>
          </div>
        </div>
      )}

      {mode === "join" && state && !state.enabled && (
        <div className="sync-join">
          <label htmlFor="sync-join-key">Sync key from your other device</label>
          <input
            id="sync-join-key"
            type="text"
            value={joinText}
            placeholder="XXXX-XXXX-XXXX-…"
            autoComplete="off"
            autoCapitalize="characters"
            spellCheck={false}
            disabled={busy}
            onChange={(event) => setJoinText(event.target.value)}
          />
          <div className="settings-actions">
            <button className="primary-button" disabled={busy || joinText.trim() === ""} onClick={() => void join()}>
              {busy ? "Joining…" : "Add this device"}
            </button>
            <button className="secondary-button" disabled={busy} onClick={() => { setMode("idle"); setJoinText(""); }}>
              Cancel
            </button>
          </div>
        </div>
      )}

      {mode === "confirm-off" && (
        <div className="sync-confirm" role="alertdialog" aria-label="Turn off sync">
          <p>
            Turn off sync on this device? Your library, My List and history stay here. The synced copy stays on the
            server for your other devices, and you can join again with the key.
          </p>
          <div className="settings-actions">
            <button className="secondary-button" disabled={busy} onClick={() => void turnOff(false)}>Turn off sync</button>
            <button className="secondary-button" disabled={busy} onClick={() => setMode("idle")}>Cancel</button>
          </div>
        </div>
      )}

      {mode === "confirm-delete" && (
        <div className="sync-confirm" role="alertdialog" aria-label="Delete synced data">
          <p>
            Delete the synced copy from the server for every device, and turn sync off here? Your other devices keep
            their own local data but stop syncing. This cannot be undone.
          </p>
          <div className="settings-actions">
            <button className="danger-button" disabled={busy} onClick={() => void turnOff(true)}>Delete synced data</button>
            <button className="secondary-button" disabled={busy} onClick={() => setMode("idle")}>Cancel</button>
          </div>
        </div>
      )}

      {error && <div className="dialog-error" role="alert">{error}</div>}

      {state && mode === "idle" && (
        <div className="settings-actions">
          {state.enabled ? (
            <>
              <button className="secondary-button" disabled={busy} onClick={() => void syncAction()}>
                {busy ? "Working…" : "Sync now"}
              </button>
              <button className="secondary-button" disabled={busy} onClick={() => void showKey()}>Show key</button>
              <button className="secondary-button" disabled={busy} onClick={() => setMode("confirm-off")}>Turn off</button>
              <button className="danger-button" disabled={busy} onClick={() => setMode("confirm-delete")}>
                Delete synced data
              </button>
            </>
          ) : (
            <>
              <button className="primary-button" disabled={busy || !state.serverUrl} onClick={() => void turnOn()}>
                {busy ? "Turning on…" : "Turn on sync"}
              </button>
              <button className="secondary-button" disabled={busy || !state.serverUrl} onClick={() => setMode("join")}>
                Add this device
              </button>
            </>
          )}
        </div>
      )}
    </div>
  );
}

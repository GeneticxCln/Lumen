import { useEffect, useState } from "react";
import {
  errorMessage,
  listProfiles,
  syncDisable,
  syncEnable,
  syncJoin,
  syncSetServer,
  syncShowKey,
  syncState,
  syncStatus,
  syncNow,
} from "../lib/tauri";
import { t } from "../i18n/index.ts";
import { describeSync, skippedWarning } from "../lib/sync";
import { MergeProfilesDialog } from "./MergeProfilesDialog";
import type { MergeCandidate } from "./MergeProfilesDialog";
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
  const [mergeCandidates, setMergeCandidates] = useState<MergeCandidate[]>([]);

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
      setError(t("sync.firstSyncFailed", { reason: errorMessage(failure) }));
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
    // Snapshotted before joining: every profile gets its own random sync key at creation (see
    // `db::create_profile`), so a profile that already existed on this device and one that just
    // arrived from the group never share a key even when they're the same person — the only way
    // to tell "arrived just now" from "was already here" is to compare profile ids across the
    // join, not anything the profiles' own data carries.
    const before = await listProfiles();
    setState(await syncJoin(joinText));
    setJoinText("");
    setMode("idle");
    await syncAfterEnable();

    const after = await listProfiles();
    const beforeIds = new Set(before.map((profile) => profile.id));
    const afterIds = new Set(after.map((profile) => profile.id));
    const stillLocal = before.filter((profile) => afterIds.has(profile.id));
    const arrived = after.filter((profile) => !beforeIds.has(profile.id));
    if (stillLocal.length > 0 && arrived.length > 0) {
      setMergeCandidates(
        stillLocal.flatMap((local) => arrived.map((remote) => ({ local, remote }))),
      );
    }
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
      setError(t("sync.key.copyFailed"));
    }
  };

  const warning = sync ? skippedWarning(sync) : null;

  return (
    <div className="sync-settings">
      <h3 className="settings-section-title">{t("sync.title")}</h3>
      {!state && !error && <p className="muted-note">{t("sync.checking")}</p>}

      {state && !state.enabled && (
        <>
          <p>{t("sync.intro")}</p>
          <p className="muted-note">{t("sync.privacy")}</p>
          <details className="sync-server">
            <summary>{state.serverUrl ? t("sync.server.summary", { url: state.serverUrl }) : t("sync.server.summaryUnset")}</summary>
            <p className="muted-note">
              {state.defaultServer && state.serverUrl
                ? t("sync.server.builtIn")
                : t("sync.server.enter")}
            </p>
            <label htmlFor="sync-server-url">{t("sync.server.label")}</label>
            <input
              id="sync-server-url"
              type="url"
              value={serverText}
              placeholder={t("sync.server.placeholder")}
              disabled={busy}
              onChange={(event) => setServerText(event.target.value)}
            />
            <div className="settings-actions">
              <button className="secondary-button" disabled={busy} onClick={() => void saveServer()}>
                {t("sync.server.save")}
              </button>
            </div>
          </details>
        </>
      )}

      {state?.enabled && (
        <>
          <p>{t("sync.on")}</p>
          {sync && <p className="muted-note">{describeSync(sync)}</p>}
          {state.serverUrl && <p className="muted-note">{t("sync.serverLine", { url: state.serverUrl })}</p>}
          {warning && <div className="dialog-error" role="alert">{warning}</div>}
        </>
      )}

      {shownKey && (
        <div className="sync-key-panel" role="region" aria-label={t("sync.key.region")}>
          <p className="sync-key" data-testid="sync-key">{shownKey}</p>
          <p className="muted-note">
            {keyIsNew ? t("sync.key.new") : t("sync.key.existing")}
          </p>
          <div className="settings-actions">
            <button className="secondary-button" onClick={() => void copyKey()}>
              {copied ? t("sync.key.copied") : t("sync.key.copy")}
            </button>
            <button className="secondary-button" onClick={() => setShownKey(null)}>
              {keyIsNew ? t("sync.key.saved") : t("sync.key.hide")}
            </button>
          </div>
        </div>
      )}

      {mode === "join" && state && !state.enabled && (
        <div className="sync-join">
          <label htmlFor="sync-join-key">{t("sync.join.label")}</label>
          <input
            id="sync-join-key"
            type="text"
            value={joinText}
            placeholder={t("sync.join.placeholder")}
            autoComplete="off"
            autoCapitalize="characters"
            spellCheck={false}
            disabled={busy}
            onChange={(event) => setJoinText(event.target.value)}
          />
          <div className="settings-actions">
            <button className="primary-button" disabled={busy || joinText.trim() === ""} onClick={() => void join()}>
              {busy ? t("sync.join.joining") : t("sync.addDevice")}
            </button>
            <button className="secondary-button" disabled={busy} onClick={() => { setMode("idle"); setJoinText(""); }}>
              {t("common.cancel")}
            </button>
          </div>
        </div>
      )}

      {mode === "confirm-off" && (
        <div className="sync-confirm" role="alertdialog" aria-label={t("sync.turnOffSync")}>
          <p>{t("sync.turnOffConfirm")}</p>
          <div className="settings-actions">
            <button className="secondary-button" disabled={busy} onClick={() => void turnOff(false)}>{t("sync.turnOffSync")}</button>
            <button className="secondary-button" disabled={busy} onClick={() => setMode("idle")}>{t("common.cancel")}</button>
          </div>
        </div>
      )}

      {mode === "confirm-delete" && (
        <div className="sync-confirm" role="alertdialog" aria-label={t("sync.deleteData")}>
          <p>{t("sync.deleteConfirm")}</p>
          <div className="settings-actions">
            <button className="danger-button" disabled={busy} onClick={() => void turnOff(true)}>{t("sync.deleteData")}</button>
            <button className="secondary-button" disabled={busy} onClick={() => setMode("idle")}>{t("common.cancel")}</button>
          </div>
        </div>
      )}

      {error && <div className="dialog-error" role="alert">{error}</div>}

      {state && mode === "idle" && (
        <div className="settings-actions">
          {state.enabled ? (
            <>
              <button className="secondary-button" disabled={busy} onClick={() => void syncAction()}>
                {busy ? t("sync.working") : t("sync.syncNow")}
              </button>
              <button className="secondary-button" disabled={busy} onClick={() => void showKey()}>{t("sync.showKey")}</button>
              <button className="secondary-button" disabled={busy} onClick={() => setMode("confirm-off")}>{t("sync.turnOff")}</button>
              <button className="danger-button" disabled={busy} onClick={() => setMode("confirm-delete")}>
                {t("sync.deleteData")}
              </button>
            </>
          ) : (
            <>
              <button className="primary-button" disabled={busy || !state.serverUrl} onClick={() => void turnOn()}>
                {busy ? t("sync.turningOn") : t("sync.turnOn")}
              </button>
              <button className="secondary-button" disabled={busy || !state.serverUrl} onClick={() => setMode("join")}>
                {t("sync.addDevice")}
              </button>
            </>
          )}
        </div>
      )}

      {mergeCandidates.length > 0 && (
        <MergeProfilesDialog
          candidates={mergeCandidates}
          onClose={() => setMergeCandidates([])}
          onMerged={() => {}}
        />
      )}
    </div>
  );
}

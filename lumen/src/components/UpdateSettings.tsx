import { useCallback, useEffect, useState } from "react";
import { errorMessage, updateCheck, updateInstall, updateRestart, updateSetMode, updateState } from "../lib/tauri";
import type { UpdateAvailable, UpdateMode, UpdateState } from "../lib/types";
import { t } from "../i18n/index.ts";

/** Lumen's version, when it last looked for a newer release, and what to do about one. */
export function UpdateSettings() {
  const [state, setState] = useState<UpdateState | null>(null);
  const [found, setFound] = useState<UpdateAvailable | null | undefined>(undefined);
  const [busy, setBusy] = useState<"check" | "install" | null>(null);
  const [installed, setInstalled] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const next = await updateState();
      setState(next);
      setInstalled((current) => current ?? next.installed);
    } catch (failure) {
      setError(errorMessage(failure));
    }
  }, []);

  useEffect(() => { void refresh(); }, [refresh]);

  const changeMode = async (mode: UpdateMode) => {
    setError(null);
    try {
      await updateSetMode(mode);
      setState((current) => (current ? { ...current, mode } : current));
    } catch (failure) {
      setError(errorMessage(failure));
    }
  };

  const check = async () => {
    setBusy("check");
    setError(null);
    try {
      setFound(await updateCheck());
      await refresh();
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setBusy(null);
    }
  };

  const install = async () => {
    setBusy("install");
    setError(null);
    try {
      setInstalled(await updateInstall());
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setBusy(null);
    }
  };

  const canInstall = state?.kind === "appImage" && found?.package != null;
  return (
    <>
      <h3 className="settings-section-title">{t("update.title")}</h3>
      <div className="setting-row">
        <div>
          <label htmlFor="setting-update-mode">{t("update.mode.label")}</label>
          <p>{state ? t("update.version", { version: state.current }) : ""}</p>
          <p>{t("update.mode.note")}</p>
          {state?.kind === "managed" && <p>{t("update.managed")}</p>}
        </div>
        <select id="setting-update-mode" value={state?.mode ?? "auto"} disabled={!state} onChange={(event) => void changeMode(event.target.value as UpdateMode)}>
          <option value="auto">{t("update.mode.auto")}</option>
          <option value="notify">{t("update.mode.notify")}</option>
          <option value="off">{t("update.mode.off")}</option>
        </select>
      </div>
      <div className="setting-row">
        <div>
          <p>{state?.lastChecked ? t("update.lastChecked", { when: new Date(state.lastChecked * 1000).toLocaleString() }) : t("update.neverChecked")}</p>
          {found === null && <p role="status">{t("update.upToDate")}</p>}
          {found && <p role="status">{t("update.available", { tag: found.tag })}</p>}
          {found && !canInstall && <p>{t("update.page", { address: found.page })}</p>}
          {found && state?.kind === "appImage" && !found.package && <p>{t("update.noPackage")}</p>}
          {installed && <p role="status">{t("update.installed", { tag: installed })}</p>}
          {error && <p className="dialog-error" role="alert">{error}</p>}
        </div>
        <div className="settings-actions">
          <button className="secondary-button" disabled={busy !== null} onClick={() => void check()}>{busy === "check" ? t("update.checking") : t("update.check")}</button>
          {canInstall && !installed && <button className="primary-button" disabled={busy !== null} onClick={() => void install()}>{busy === "install" ? t("update.installing") : t("update.install", { tag: found?.tag ?? "" })}</button>}
          {installed && <button className="primary-button" onClick={() => void updateRestart().catch((failure) => setError(errorMessage(failure)))}>{t("update.restart")}</button>}
        </div>
      </div>
    </>
  );
}

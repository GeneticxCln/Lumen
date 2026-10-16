import { useCallback, useEffect, useState } from "react";
import { errorMessage, updateCheck, updateInstall, updateRestart, updateSetMode, updateState } from "../lib/tauri";
import type { UpdateAvailable, UpdateMode, UpdateState } from "../lib/types";
import { t } from "../i18n/index.ts";

/**
 * Updating Lumen from its latest release. One button does the whole job: it looks up the newest
 * released tag and, if it is newer than this version and this copy can replace itself, downloads,
 * checks and installs it, then offers a restart.
 */
export function UpdateSettings({ state: given, onInstalled }: { state?: UpdateState | null; onInstalled?: (tag: string) => void }) {
  const [state, setState] = useState<UpdateState | null>(given ?? null);
  const [found, setFound] = useState<UpdateAvailable | null | undefined>(undefined);
  const [working, setWorking] = useState<string | null>(null);
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

  const update = async () => {
    setError(null);
    setFound(undefined);
    setWorking(t("update.working.checking"));
    try {
      const latest = await updateCheck();
      setFound(latest);
      await refresh();
      if (latest && state?.kind === "appImage" && latest.package) {
        setWorking(t("update.working.installing", { tag: latest.tag }));
        const tag = await updateInstall();
        setInstalled(tag);
        onInstalled?.(tag);
      }
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setWorking(null);
    }
  };

  const canInstall = state?.kind === "appImage";
  return (
    <>
      <h3 className="settings-section-title">{t("update.title")}</h3>
      <div className="setting-row">
        <div>
          <p>{state?.lastChecked ? t("update.lastChecked", { when: new Date(state.lastChecked * 1000).toLocaleString() }) : t("update.neverChecked")}</p>
          {working && <p role="status">{working}</p>}
          {found === null && !working && <p role="status">{t("update.upToDate")}</p>}
          {found && !installed && <p role="status">{t("update.available", { tag: found.tag })}</p>}
          {found && !installed && !working && !(canInstall && found.package) && <p>{t("update.page", { address: found.page })}</p>}
          {found && canInstall && !found.package && !working && <p>{t("update.noPackage")}</p>}
          {installed && <p role="status">{t("update.installed", { tag: installed })}</p>}
          {state?.kind === "managed" && <p className="muted-note">{t("update.managed")}</p>}
          {error && <p className="dialog-error" role="alert">{error}</p>}
        </div>
        <div className="settings-actions">
          {installed
            ? <button className="primary-button" onClick={() => void updateRestart().catch((failure) => setError(errorMessage(failure)))}>{t("update.restart")}</button>
            : <button className="primary-button" disabled={working !== null || !state} onClick={() => void update()}>{canInstall ? t("update.action.install") : t("update.action.check")}</button>}
        </div>
      </div>
      <div className="setting-row">
        <div>
          <label htmlFor="setting-update-mode">{t("update.mode.label")}</label>
          <p>{t("update.mode.note")}</p>
        </div>
        <select id="setting-update-mode" value={state?.mode ?? "auto"} disabled={!state} onChange={(event) => void changeMode(event.target.value as UpdateMode)}>
          <option value="auto">{t("update.mode.auto")}</option>
          <option value="notify">{t("update.mode.notify")}</option>
          <option value="off">{t("update.mode.off")}</option>
        </select>
      </div>
    </>
  );
}

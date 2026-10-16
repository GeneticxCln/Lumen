import { useEffect, useState } from "react";
import { errorMessage, updateState } from "../lib/tauri";
import type { UpdateState } from "../lib/types";
import { t } from "../i18n/index.ts";
import { UpdateSettings } from "./UpdateSettings";

const REPOSITORY = "https://github.com/GeneticxCln/Lumen";

/** What Lumen is, which version this is, its license, and the updater. */
export function AboutLumen() {
  const [state, setState] = useState<UpdateState | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    updateState().then((next) => { if (active) setState(next); }).catch(() => {});
    return () => { active = false; };
  }, []);

  const openRepository = async () => {
    setError(null);
    try {
      const { openUrl } = await import("@tauri-apps/plugin-opener");
      await openUrl(REPOSITORY);
    } catch (failure) {
      setError(t("about.openFailed", { reason: errorMessage(failure) }));
    }
  };

  return (
    <>
      <h3 className="settings-section-title">{t("about.title")}</h3>
      <div className="setting-row">
        <div>
          <p><strong>{state ? t("about.version", { version: state.current }) : ""}</strong></p>
          <p>{t("about.tagline")}</p>
          <p>{t("about.license")}</p>
        </div>
      </div>
      <div className="setting-row">
        <div>
          <p>{t("about.source")}</p>
          <p className="muted-note">{REPOSITORY}</p>
          {error && <p className="dialog-error" role="alert">{error}</p>}
        </div>
        <button className="secondary-button" onClick={() => void openRepository()}>{t("about.sourceOpen")}</button>
      </div>
      <UpdateSettings state={state} />
    </>
  );
}

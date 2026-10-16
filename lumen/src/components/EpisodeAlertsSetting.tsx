import { useState } from "react";
import { episodeAlertsEnabled, setEpisodeAlertsEnabled } from "../hooks/useEpisodeAlerts";
import { errorMessage } from "../lib/tauri";
import { t } from "../i18n/index.ts";

/** The switch for desktop notifications about new episodes of followed series. Off by default. */
export function EpisodeAlertsSetting() {
  const [enabled, setEnabled] = useState(episodeAlertsEnabled());
  const [message, setMessage] = useState<string | null>(null);

  const change = async (next: boolean) => {
    setMessage(null);
    try {
      const done = await setEpisodeAlertsEnabled(next);
      if (done) setEnabled(next);
      else setMessage(t("alerts.denied"));
    } catch (failure) {
      setMessage(errorMessage(failure));
    }
  };

  return (
    <div className="setting-row">
      <div>
        <label htmlFor="setting-episode-alerts">{t("alerts.label")}</label>
        <p>{t("alerts.description")}</p>
        {message && <p className="dialog-error" role="alert">{message}</p>}
      </div>
      <input
        id="setting-episode-alerts"
        type="checkbox"
        role="switch"
        checked={enabled}
        onChange={(event) => void change(event.target.checked)}
      />
    </div>
  );
}

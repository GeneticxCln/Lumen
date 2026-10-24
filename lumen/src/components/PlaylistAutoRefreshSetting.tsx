import { useEffect, useState } from "react";
import { errorMessage, getSettings, setSetting } from "../lib/tauri";
import { t } from "../i18n/index.ts";

/** Hours between auto-refreshes offered in the selector; "0" is off. A playlist can override
 * this default for itself (see the per-source selector in the sources list). */
const HOUR_CHOICES = ["0", "6", "12", "24"] as const;

/**
 * How often a URL-imported playlist re-fetches its source on its own, in the background, unless
 * a playlist overrides this for itself. Off by default. Saved immediately.
 */
export function PlaylistAutoRefreshSetting() {
  const [hours, setHours] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    getSettings()
      .then((settings) => { if (active) setHours(settings.playlist_auto_refresh_hours || "0"); })
      .catch((failure) => { if (active) setError(errorMessage(failure)); });
    return () => { active = false; };
  }, []);

  const change = async (next: string) => {
    const previous = hours;
    setHours(next);
    setError(null);
    try {
      await setSetting("playlist_auto_refresh_hours", next);
    } catch (failure) {
      setHours(previous);
      setError(errorMessage(failure));
    }
  };

  return (
    <div className="setting-row">
      <div>
        <label htmlFor="setting-playlist-auto-refresh">{t("autoRefresh.label")}</label>
        <p>{t("autoRefresh.description")}</p>
        {error && <p className="dialog-error" role="alert">{error}</p>}
      </div>
      <select
        id="setting-playlist-auto-refresh"
        value={hours ?? "0"}
        disabled={hours === null}
        onChange={(event) => void change(event.target.value)}
      >
        {HOUR_CHOICES.map((choice) => (
          <option key={choice} value={choice}>
            {choice === "0" ? t("autoRefresh.off") : t("autoRefresh.everyHours", { hours: choice })}
          </option>
        ))}
      </select>
    </div>
  );
}

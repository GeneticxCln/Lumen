import { useEffect, useState } from "react";
import { errorMessage, subtitlesSetOnline, subtitlesSetService, subtitlesState } from "../lib/tauri";
import type { SubtitleState } from "../lib/types";
import { t } from "../i18n/index.ts";

function serviceLabel(id: string): string {
  return id === "gestdown" ? t("subtitles.service.gestdown") : t("subtitles.service.mirror");
}

/**
 * The switch for looking subtitles up online, and which services to use. Off until the user turns
 * it on here or agrees in the player's Find subtitles dialog. Saved at once.
 */
export function OnlineSubtitlesSettings() {
  const [state, setState] = useState<SubtitleState | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    subtitlesState()
      .then((next) => { if (active) setState(next); })
      .catch((failure) => { if (active) setError(errorMessage(failure)); });
    return () => { active = false; };
  }, []);

  const change = async (action: () => Promise<unknown>) => {
    setError(null);
    try {
      await action();
      setState(await subtitlesState());
    } catch (failure) {
      setError(t("subtitles.settings.failed", { reason: errorMessage(failure) }));
    }
  };

  return (
    <div className="form-group online-subtitles">
      <h3 className="settings-section-title">{t("subtitles.settings.title")}</h3>
      <p className="muted-note">{t("subtitles.settings.intro")}</p>
      <div className="setting-row">
        <div>
          <label htmlFor="setting-subtitles-online">{t("subtitles.settings.online")}</label>
          <p className="muted-note">{t("subtitles.consent.warning")}</p>
        </div>
        <input
          id="setting-subtitles-online"
          type="checkbox"
          role="switch"
          checked={state?.online === true}
          disabled={state === null}
          onChange={(event) => void change(() => subtitlesSetOnline(event.target.checked))}
        />
      </div>
      {state?.online && (
        <fieldset className="subtitle-services">
          <legend>{t("subtitles.settings.services")}</legend>
          {state.services.map((service) => (
            <label key={service.id} className="subtitle-service">
              <input
                type="checkbox"
                checked={service.enabled}
                onChange={(event) => void change(() => subtitlesSetService(service.id, event.target.checked))}
              />
              {serviceLabel(service.id)}
            </label>
          ))}
        </fieldset>
      )}
      {error && <div className="dialog-error" role="alert">{error}</div>}
    </div>
  );
}

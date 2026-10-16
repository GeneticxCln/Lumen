import { useEffect, useState } from "react";
import { errorMessage, getPlayerPrefs, setPlayerPrefs } from "../lib/tauri";
import { clampScale, LANGUAGES, MAX_SUB_SCALE, MIN_SUB_SCALE } from "../lib/languages";
import type { PlayerPrefs } from "../lib/types";
import { t } from "../i18n/index.ts";
import { Rich } from "../i18n/Rich.tsx";

/**
 * Audio and subtitle preferences for the active profile. Saved as soon as they change, and applied
 * to the player that is running, so a size or position change is visible straight away.
 */
export function PlayerPrefsSettings() {
  const [prefs, setPrefs] = useState<PlayerPrefs | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    getPlayerPrefs()
      .then((loaded) => { if (active) setPrefs(loaded); })
      .catch((failure) => { if (active) setError(errorMessage(failure)); });
    return () => { active = false; };
  }, []);

  const change = async (next: PlayerPrefs) => {
    const previous = prefs;
    setPrefs(next);
    setError(null);
    try {
      setPrefs(await setPlayerPrefs(next));
    } catch (failure) {
      setPrefs(previous);
      setError(errorMessage(failure));
    }
  };

  if (!prefs) return error ? <div className="dialog-error" role="alert">{error}</div> : <p className="muted-note">{t("common.loading")}</p>;

  return <div className="form-group player-prefs">
    <h3 className="settings-section-title">{t("prefs.title")}</h3>
    <p className="muted-note">{t("prefs.intro")}</p>
    <div className="player-prefs-grid">
      <label htmlFor="pref-audio-lang">{t("prefs.audioLanguage")}</label>
      <select id="pref-audio-lang" value={prefs.audioLang} onChange={(event) => void change({ ...prefs, audioLang: event.target.value })}>
        <option value="">{t("common.playerDefault")}</option>
        {LANGUAGES.map((language) => <option key={language.code} value={language.code}>{language.label}</option>)}
      </select>

      <label htmlFor="pref-subtitles">{t("prefs.subtitles")}</label>
      <select id="pref-subtitles" value={prefs.subtitles} onChange={(event) => void change({ ...prefs, subtitles: event.target.value })}>
        <option value="auto">{t("common.playerDefault")}</option>
        <option value="off">{t("common.off")}</option>
        {LANGUAGES.map((language) => <option key={language.code} value={language.code}>{language.label}</option>)}
      </select>

      <label htmlFor="pref-sub-scale">{t("prefs.subtitleSize")} <span className="muted-note">{Math.round(prefs.subScale * 100)}%</span></label>
      <input id="pref-sub-scale" type="range" min={MIN_SUB_SCALE} max={MAX_SUB_SCALE} step={0.1} value={prefs.subScale}
        onChange={(event) => setPrefs({ ...prefs, subScale: clampScale(Number(event.target.value)) })}
        onPointerUp={() => void change(prefs)} onKeyUp={() => void change(prefs)} />

      <label htmlFor="pref-sub-pos">{t("prefs.subtitlePosition")} <span className="muted-note">{prefs.subPos === 100 ? t("prefs.positionBottom") : prefs.subPos === 0 ? t("prefs.positionTop") : t("prefs.positionPercent", { percent: prefs.subPos })}</span></label>
      <input id="pref-sub-pos" type="range" min={0} max={100} step={1} value={prefs.subPos}
        onChange={(event) => setPrefs({ ...prefs, subPos: Number(event.target.value) })}
        onPointerUp={() => void change(prefs)} onKeyUp={() => void change(prefs)} />
    </div>
    <p className="muted-note"><Rich id="prefs.sideloadNote" tags={{ code: (text) => <code>{text}</code> }} /></p>
    {error && <div className="dialog-error" role="alert">{error}</div>}
  </div>;
}

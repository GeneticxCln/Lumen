import { useEffect, useState } from "react";
import { errorMessage, getPlayerPrefs, setPlayerPrefs } from "../lib/tauri";
import { AUTOPLAY_COUNTDOWNS, clampScale, LANGUAGES, MAX_SUB_SCALE, MIN_SUB_SCALE, SEEK_STEPS } from "../lib/languages";
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
    <h3 className="settings-section-title">{t("prefs.styleTitle")}</h3>
    <div className="player-prefs-grid">
      <label htmlFor="pref-sub-color">{t("prefs.subtitleColor")}</label>
      <input id="pref-sub-color" type="color" value={prefs.subColor}
        onChange={(event) => setPrefs({ ...prefs, subColor: event.target.value })} onBlur={() => void change(prefs)} />

      <label htmlFor="pref-sub-outline">{t("prefs.subtitleOutline")}</label>
      <input id="pref-sub-outline" type="color" value={prefs.subOutlineColor}
        onChange={(event) => setPrefs({ ...prefs, subOutlineColor: event.target.value })} onBlur={() => void change(prefs)} />

      <label htmlFor="pref-sub-back-on">{t("prefs.subtitleBackground")}</label>
      <div className="player-prefs-inline">
        <input id="pref-sub-back-on" type="checkbox" checked={prefs.subBackColor !== ""} aria-label={t("prefs.subtitleBackgroundOn")}
          onChange={(event) => void change({ ...prefs, subBackColor: event.target.checked ? "#000000" : "" })} />
        <input type="color" value={prefs.subBackColor || "#000000"} disabled={prefs.subBackColor === ""} aria-label={t("prefs.subtitleBackground")}
          onChange={(event) => setPrefs({ ...prefs, subBackColor: event.target.value })} onBlur={() => void change(prefs)} />
      </div>

      <label htmlFor="pref-sub-bold">{t("prefs.subtitleBold")}</label>
      <input id="pref-sub-bold" type="checkbox" checked={prefs.subBold} onChange={(event) => void change({ ...prefs, subBold: event.target.checked })} />

      <label htmlFor="pref-sub-font">{t("prefs.subtitleFont")}</label>
      <input id="pref-sub-font" type="text" value={prefs.subFont} maxLength={64} placeholder={t("prefs.subtitleFontPlaceholder")} spellCheck={false}
        onChange={(event) => setPrefs({ ...prefs, subFont: event.target.value })} onBlur={() => void change(prefs)} />
    </div>
    <div className="sub-preview" role="img" aria-label={t("prefs.previewLabel")}
      style={{
        color: prefs.subColor,
        fontWeight: prefs.subBold ? 700 : 400,
        fontFamily: prefs.subFont || "sans-serif",
        background: prefs.subBackColor ? `${prefs.subBackColor}cc` : "transparent",
        textShadow: [[-1, -1], [1, -1], [-1, 1], [1, 1], [0, 2]].map(([x, y]) => `${x}px ${y}px 0 ${prefs.subOutlineColor}`).join(", "),
      }}>
      {t("prefs.previewText")}
    </div>
    <p className="muted-note">{t("prefs.styleNote")}</p>

    <h3 className="settings-section-title">{t("prefs.playerTitle")}</h3>
    <div className="player-prefs-grid">
      <label htmlFor="pref-seek-step">{t("prefs.seekStep")}</label>
      <select id="pref-seek-step" value={prefs.seekStep} onChange={(event) => void change({ ...prefs, seekStep: Number(event.target.value) })}>
        {SEEK_STEPS.map((seconds) => <option key={seconds} value={seconds}>{t("prefs.seconds", { count: seconds })}</option>)}
      </select>

      <label htmlFor="pref-autoplay">{t("prefs.autoplay")}</label>
      <select id="pref-autoplay" value={prefs.autoplayCountdown} onChange={(event) => void change({ ...prefs, autoplayCountdown: Number(event.target.value) })}>
        {AUTOPLAY_COUNTDOWNS.map((seconds) => <option key={seconds} value={seconds}>{seconds === 0 ? t("prefs.autoplayOff") : t("prefs.autoplayAfter", { count: seconds })}</option>)}
      </select>
    </div>
    <p className="muted-note"><Rich id="prefs.sideloadNote" tags={{ code: (text) => <code>{text}</code> }} /></p>
    {error && <div className="dialog-error" role="alert">{error}</div>}
  </div>;
}

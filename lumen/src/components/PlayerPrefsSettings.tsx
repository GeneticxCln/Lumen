import { useEffect, useState } from "react";
import { errorMessage, getPlayerPrefs, setPlayerPrefs } from "../lib/tauri";
import { clampScale, LANGUAGES, MAX_SUB_SCALE, MIN_SUB_SCALE } from "../lib/languages";
import type { PlayerPrefs } from "../lib/types";

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

  if (!prefs) return error ? <div className="dialog-error" role="alert">{error}</div> : <p className="muted-note">Loading…</p>;

  return <div className="form-group player-prefs">
    <h3 className="settings-section-title">Audio and subtitles</h3>
    <p className="muted-note">For the profile you are using. Language choices apply to the next thing you play.</p>
    <div className="player-prefs-grid">
      <label htmlFor="pref-audio-lang">Preferred audio language</label>
      <select id="pref-audio-lang" value={prefs.audioLang} onChange={(event) => void change({ ...prefs, audioLang: event.target.value })}>
        <option value="">Player default</option>
        {LANGUAGES.map((language) => <option key={language.code} value={language.code}>{language.label}</option>)}
      </select>

      <label htmlFor="pref-subtitles">Subtitles</label>
      <select id="pref-subtitles" value={prefs.subtitles} onChange={(event) => void change({ ...prefs, subtitles: event.target.value })}>
        <option value="auto">Player default</option>
        <option value="off">Off</option>
        {LANGUAGES.map((language) => <option key={language.code} value={language.code}>{language.label}</option>)}
      </select>

      <label htmlFor="pref-sub-scale">Subtitle size <span className="muted-note">{Math.round(prefs.subScale * 100)}%</span></label>
      <input id="pref-sub-scale" type="range" min={MIN_SUB_SCALE} max={MAX_SUB_SCALE} step={0.1} value={prefs.subScale}
        onChange={(event) => setPrefs({ ...prefs, subScale: clampScale(Number(event.target.value)) })}
        onPointerUp={() => void change(prefs)} onKeyUp={() => void change(prefs)} />

      <label htmlFor="pref-sub-pos">Subtitle position <span className="muted-note">{prefs.subPos === 100 ? "bottom" : prefs.subPos === 0 ? "top" : `${prefs.subPos}%`}</span></label>
      <input id="pref-sub-pos" type="range" min={0} max={100} step={1} value={prefs.subPos}
        onChange={(event) => setPrefs({ ...prefs, subPos: Number(event.target.value) })}
        onPointerUp={() => void change(prefs)} onKeyUp={() => void change(prefs)} />
    </div>
    <p className="muted-note">A subtitle file saved next to a local video (for example <code>Movie.srt</code> beside <code>Movie.mkv</code>) is loaded and shown automatically, and takes priority over subtitles inside the video.</p>
    {error && <div className="dialog-error" role="alert">{error}</div>}
  </div>;
}

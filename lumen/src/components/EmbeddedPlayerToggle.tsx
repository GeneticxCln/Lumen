import { useEffect, useState } from "react";
import { embeddedPlayerSupport, errorMessage, getSettings, setSetting } from "../lib/tauri";
import type { EmbeddedSupport } from "../lib/tauri";

/**
 * Experimental: play video inside the Lumen window instead of a separate mpv window. It needs an
 * X11 window (a native Wayland window cannot host another program's video), so the switch explains
 * itself when it is unavailable. Saved at once; it applies the next time playback starts.
 */
export function EmbeddedPlayerToggle() {
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [support, setSupport] = useState<EmbeddedSupport | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    Promise.all([getSettings(), embeddedPlayerSupport()])
      .then(([settings, supported]) => {
        if (!active) return;
        setEnabled(settings.embed_player === "true");
        setSupport(supported);
      })
      .catch((failure) => { if (active) setError(errorMessage(failure)); });
    return () => { active = false; };
  }, []);

  const change = async (next: boolean) => {
    const previous = enabled;
    setEnabled(next);
    setError(null);
    try {
      await setSetting("embed_player", next ? "true" : "false");
    } catch (failure) {
      setEnabled(previous);
      setError(errorMessage(failure));
    }
  };

  const unavailable = support !== null && !support.supported;
  return (
    <div className="setting-row">
      <div>
        <label htmlFor="setting-embed-player">Play video inside the Lumen window (experimental)</label>
        <p>
          Shows the video above the controls in this window instead of opening a separate mpv window.
          Takes effect the next time playback starts.
        </p>
        {unavailable && <p className="muted-note">{support?.reason}</p>}
        {error && <p className="dialog-error" role="alert">{error}</p>}
      </div>
      <input
        id="setting-embed-player"
        type="checkbox"
        role="switch"
        checked={enabled === true && !unavailable}
        disabled={enabled === null || unavailable}
        onChange={(event) => void change(event.target.checked)}
      />
    </div>
  );
}

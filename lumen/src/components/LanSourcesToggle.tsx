import { useEffect, useState } from "react";
import { errorMessage, getSettings, setSetting } from "../lib/tauri";

/**
 * Whether Lumen may fetch playlists from addresses on this computer or its network. Off by
 * default, so a web page or playlist cannot aim the app at a router or a local service; turn it
 * on for a playlist you host yourself (a NAS, Jellyfin on this machine). Saved immediately.
 */
export function LanSourcesToggle() {
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    getSettings()
      .then((settings) => { if (active) setEnabled(settings.allow_lan_sources === "true"); })
      .catch((failure) => { if (active) setError(errorMessage(failure)); });
    return () => { active = false; };
  }, []);

  const change = async (next: boolean) => {
    const previous = enabled;
    setEnabled(next);
    setError(null);
    try {
      await setSetting("allow_lan_sources", next ? "true" : "false");
    } catch (failure) {
      setEnabled(previous);
      setError(errorMessage(failure));
    }
  };

  return (
    <div className="setting-row">
      <div>
        <label htmlFor="setting-allow-lan-sources">Allow sources on my local network</label>
        <p>
          Lets Lumen import playlists from this computer or your home network (for example a NAS or a
          media server). Leave this off unless you host a playlist yourself.
        </p>
        {error && <p className="dialog-error" role="alert">{error}</p>}
      </div>
      <input
        id="setting-allow-lan-sources"
        type="checkbox"
        role="switch"
        checked={enabled === true}
        disabled={enabled === null}
        onChange={(event) => void change(event.target.checked)}
      />
    </div>
  );
}

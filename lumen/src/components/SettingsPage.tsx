import { useState, useEffect, useRef } from "react";
import type { CSSProperties } from "react";
import { getSettings, setSetting, setTmdbApiKey, tmdbStatus, enrichTmdb, clearTmdbEnrichment, clearLocalThumbnailCache, localPosterCandidates, errorMessage } from "../lib/tauri";
import type { Channel, Playlist, PosterCandidate, Profile, TmdbStatus } from "../lib/types";
import { defaultPreferences, themes, applyTheme } from "../lib/discovery";
import type { Preferences } from "../lib/discovery";
import { LocalBrowser } from "./LocalBrowser";
import { ProfileSettings } from "./ProfileSettings";
import { MediaShelf } from "./MediaShelf";
import { Icon } from "./Icon";
import type { IconName } from "./Icon";

export type SettingsSection = "account" | "sources" | "files" | "mylist" | "metadata" | "playback" | "appearance" | "shortcuts";

interface SettingsPageProps {
  preferences: Preferences;
  onPreferencesChange: (preferences: Preferences) => void;
  playlists: Playlist[];
  myList: Channel[];
  onAddSource: () => void;
  onDeleteSource: (id: number) => Promise<void>;
  onRefresh: () => void;
  onPlayFile: (path: string, name?: string) => void | Promise<void>;
  onPlayChannel: (channel: Channel) => void;
  onDetails: (channel: Channel) => void;
  onToggleFavorite: (id: number) => void;
  onBrowseDiscovery: () => void;
  loadError: string | null;
  section: SettingsSection;
  onSectionChange: (section: SettingsSection) => void;
  profiles: Profile[];
  activeProfileId: number | null;
  onProfilesChanged: () => void | Promise<void>;
}

// Must match the frame times the backends accept when saving.
const posterFrameChoices = [
  { seconds: "0", label: "Start of video" },
  { seconds: "2", label: "2 seconds (default)" },
  { seconds: "5", label: "5 seconds" },
  { seconds: "10", label: "10 seconds" },
  { seconds: "30", label: "30 seconds" },
  { seconds: "60", label: "1 minute" },
];

const sections: { id: SettingsSection; label: string; description: string; icon: IconName; group: string }[] = [
  { id: "account", label: "Account", description: "Profiles on this device", icon: "profile", group: "ACCOUNT" },
  { id: "sources", label: "Sources", description: "Manage your connected playlists", icon: "grid", group: "YOUR LIBRARY" },
  { id: "files", label: "Local files", description: "Browse media on this device", icon: "folder", group: "YOUR LIBRARY" },
  { id: "mylist", label: "My List", description: "Your saved titles, all together", icon: "bookmark", group: "YOUR LIBRARY" },
  { id: "metadata", label: "Metadata", description: "TMDB posters, genres and years", icon: "film", group: "YOUR LIBRARY" },
  { id: "playback", label: "Playback", description: "Fine-tune your viewing experience", icon: "play", group: "PREFERENCES" },
  { id: "appearance", label: "Appearance", description: "Make Lumen feel like you", icon: "palette", group: "PREFERENCES" },
  { id: "shortcuts", label: "Keyboard shortcuts", description: "Stay in control without a mouse", icon: "keyboard", group: "PREFERENCES" },
];

export function SettingsPage({
  preferences,
  onPreferencesChange,
  playlists,
  myList,
  onAddSource,
  onDeleteSource,
  onRefresh,
  onPlayFile,
  onPlayChannel,
  onDetails,
  onToggleFavorite,
  onBrowseDiscovery,
  loadError,
  section,
  onSectionChange,
  profiles,
  activeProfileId,
  onProfilesChanged,
}: SettingsPageProps) {
  const preferencesRef = useRef(preferences);
  preferencesRef.current = preferences;
  const [hwdec, setHwdec] = useState("auto-safe");
  const [cache, setCache] = useState("yes");
  const [extraArgs, setExtraArgs] = useState("");
  const [draft, setDraft] = useState(preferences);
  const [saving, setSaving] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [confirmId, setConfirmId] = useState<number | null>(null);
  const [loadRevision, setLoadRevision] = useState(0);
  const [engineError, setEngineError] = useState<string | null>(null);
  const [savedEngine, setSavedEngine] = useState({ hwdec: "auto-safe", cache: "yes", extraArgs: "" });
  const appearanceKeys = ["theme", "compactCards", "reduceMotion", "personalizedRows", "hideUnavailable"] as const;
  const draftDirty = section === "appearance" ? appearanceKeys.some((key) => draft[key] !== preferences[key])
    : section === "shortcuts" ? draft.keyboardShortcuts !== preferences.keyboardShortcuts
    : section === "playback" ? hwdec !== savedEngine.hwdec || cache !== savedEngine.cache || extraArgs.trim() !== savedEngine.extraArgs || draft.defaultVolume !== preferences.defaultVolume : false;

  // --- TMDB metadata -------------------------------------------------------
  const [tmdbKey, setTmdbKey] = useState("");
  const [tmdb, setTmdb] = useState<TmdbStatus | null>(null);
  const [tmdbBusy, setTmdbBusy] = useState(false);
  const [tmdbNotice, setTmdbNotice] = useState<string | null>(null);
  const [posterFrame, setPosterFrame] = useState("2");
  const [framePath, setFramePath] = useState("");
  const [frameCandidates, setFrameCandidates] = useState<PosterCandidate[] | null>(null);
  const [frameCandidatesBusy, setFrameCandidatesBusy] = useState(false);

  useEffect(() => {
    if (section !== "metadata") return;
    let active = true;
    tmdbStatus()
      .then((status) => { if (active) setTmdb(status); })
      .catch((failure) => { if (active) setError(errorMessage(failure)); });
    getSettings()
      .then((settings) => { if (active) setPosterFrame(settings.poster_frame_seconds); })
      .catch((failure) => { if (active) setError(errorMessage(failure)); });
    return () => { active = false; };
  }, [section]);

  const loadFrameCandidates = async () => {
    const path = framePath.trim();
    if (!path) return;
    setFrameCandidatesBusy(true);
    setError(null);
    try {
      setFrameCandidates(await localPosterCandidates(path));
    } catch (failure) {
      setFrameCandidates(null);
      setError(errorMessage(failure));
    } finally {
      setFrameCandidatesBusy(false);
    }
  };

  const savePosterFrame = async (seconds: string) => {
    const previous = posterFrame;
    setPosterFrame(seconds);
    setError(null);
    try {
      await setSetting("poster_frame_seconds", seconds);
      clearLocalThumbnailCache();
    } catch (failure) {
      setPosterFrame(previous);
      setError(errorMessage(failure));
    }
  };

  const saveTmdbKey = async () => {
    setTmdbBusy(true);
    setError(null);
    setTmdbNotice(null);
    try {
      await setTmdbApiKey(tmdbKey.trim());
      setTmdbKey("");
      const status = await tmdbStatus();
      setTmdb(status);
      setTmdbNotice(status.configured ? "API key saved on this device." : "API key cleared.");
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setTmdbBusy(false);
    }
  };

  const runEnrich = async (force: boolean) => {
    setTmdbBusy(true);
    setError(null);
    setTmdbNotice(null);
    try {
      const result = await enrichTmdb(60, force);
      setTmdb(await tmdbStatus());
      setTmdbNotice(`Checked ${result.matched + result.skipped} titles, matched ${result.matched} · ${result.remaining} not enriched yet.`);
      onRefresh();
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setTmdbBusy(false);
    }
  };

  const clearTmdbMetadata = async () => {
    setTmdbBusy(true);
    setError(null);
    setTmdbNotice(null);
    try {
      const { cleared } = await clearTmdbEnrichment();
      setTmdb(await tmdbStatus());
      setTmdbNotice(`Cleared metadata from ${cleared} titles.`);
      onRefresh();
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setTmdbBusy(false);
    }
  };

  useEffect(() => {
    if (section !== "appearance") return;
    const preview = () => applyTheme(draft.theme);
    preview();
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    media.addEventListener("change", preview);
    return () => { media.removeEventListener("change", preview); applyTheme(preferencesRef.current.theme); };
  }, [draft.theme, section]);

  useEffect(() => {
    let active = true;
    setDraft(preferencesRef.current);
    setLoading(true);
    getSettings()
      .then((settings) => {
        if (!active) return;
        setHwdec(settings.hwdec || "auto-safe");
        setCache(settings.cache || "yes");
        setExtraArgs(settings.extra_mpv_args || "");
        setSavedEngine({ hwdec: settings.hwdec || "auto-safe", cache: settings.cache || "yes", extraArgs: settings.extra_mpv_args || "" });
        setEngineError(null);
      })
      .catch((failure) => {
        if (active) setEngineError(errorMessage(failure));
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, [loadRevision]);

  const update = <K extends keyof Preferences>(key: K, value: Preferences[K]) => {
    setDraft((previous) => ({ ...previous, [key]: value }));
    setNotice(null);
  };

  const save = async () => {
    setSaving(true);
    setNotice(null);
    setError(null);
    try {
      let next = { ...preferences };
      if (section === "playback") {
        await setSetting("hwdec", hwdec);
        await setSetting("cache", cache);
        await setSetting("extra_mpv_args", extraArgs.trim());
        setSavedEngine({ hwdec, cache, extraArgs: extraArgs.trim() });
        next.defaultVolume = draft.defaultVolume;
      } else if (section === "appearance") {
        next = { ...next, theme: draft.theme, compactCards: draft.compactCards, reduceMotion: draft.reduceMotion, personalizedRows: draft.personalizedRows, hideUnavailable: draft.hideUnavailable };
      } else if (section === "shortcuts") next.keyboardShortcuts = draft.keyboardShortcuts;
      localStorage.setItem("lumen.preferences", JSON.stringify(next));
      onPreferencesChange(next);
      setNotice(section === "playback" ? "Playback settings saved. Stop playback and play again to apply engine changes." : "Preferences saved on this device.");
    } catch (failure) {
      setError(`Could not save all settings: ${errorMessage(failure)}. Retry to complete the save.`);
    } finally {
      setSaving(false);
    }
  };

  const toggle = (
    key: "compactCards" | "reduceMotion" | "hideUnavailable" | "personalizedRows" | "keyboardShortcuts",
    label: string,
    description: string
  ) => (
    <div className="setting-row">
      <div>
        <label htmlFor={`setting-${key}`}>{label}</label>
        <p>{description}</p>
      </div>
      <input
        id={`setting-${key}`}
        type="checkbox"
        role="switch"
        checked={draft[key]}
        onChange={(event) => update(key, event.target.checked)}
      />
    </div>
  );

  const active = sections.find((item) => item.id === section) || sections[0];

  return (
    <div className="settings-page">
      <aside className="settings-rail" aria-label="Settings sections">
        <p className="eyebrow">MAKE IT YOURS</p>
        <h1 className="settings-rail-title">Settings</h1>
        <p className="settings-rail-intro">A little fine-tuning. A better experience.</p>
        <nav>
          {sections.map((item, index) => (
            <div key={item.id}>{(index === 0 || sections[index - 1].group !== item.group) && <p className="settings-nav-group">{item.group}</p>}<button
              key={item.id}
              className={section === item.id ? "active" : ""}
              aria-current={section === item.id ? "true" : undefined}
              onClick={() => {
                onSectionChange(item.id);
                setNotice(null);
                setConfirmId(null);
              }}
            >
              <Icon name={item.icon} size={18} /><span>{item.label}</span><Icon name="chevron" size={14} />
            </button></div>
          ))}
        </nav>
        <div className="settings-rail-actions">
          <button className="primary-button" onClick={onAddSource}>
            <Icon name="plus" size={17} />Add source
          </button>
          <button className="secondary-button" onClick={onRefresh}>
            <Icon name="refresh" size={15} />Refresh library
          </button>
        </div>
        <div className="settings-device"><Icon name="monitor" size={17} /><div><strong>This device</strong><span>Preferences stay on your computer</span></div></div>
      </aside>

      <section className="settings-panel" role="region" aria-label={active.label}>
        <header className="settings-panel-head">
          <div className="settings-breadcrumb">Settings <Icon name="chevron" size={12} /> {active.label}</div>
          <div className="settings-title-row"><span className="settings-title-icon"><Icon name={active.icon} size={23} /></span><div><h2>{active.label}</h2><p>{active.description}</p></div></div>
        </header>

        {error && (
          <div className="dialog-error" role="alert">
            {error}{" "}
            <button className="icon-button" onClick={() => setLoadRevision((value) => value + 1)} aria-label="Reload settings">
              <Icon name="refresh" size={16} />
            </button>
          </div>
        )}
        {section === "playback" && engineError && <div className="dialog-error" role="alert">Playback settings could not be loaded: {engineError}<button className="icon-button" aria-label="Retry loading playback settings" onClick={() => setLoadRevision((value) => value + 1)}><Icon name="refresh" size={16} /></button></div>}
        {(section === "sources" || section === "files" || section === "mylist") && loadError && <div className="dialog-error" role="alert">{loadError}</div>}

        {section === "account" && (
          <>
            <h3 className="settings-section-title">Who is watching</h3>
            <p>
              Each profile keeps its own watch history and My List on this device. The library of sources
              and its channels is shared, and nothing is sent anywhere.
            </p>
            <ProfileSettings
              profiles={profiles}
              activeProfileId={activeProfileId}
              onProfilesChanged={onProfilesChanged}
            />
          </>
        )}

        {section === "sources" && (
          <>
            <div className="settings-actions settings-actions-top">
              <h3 className="settings-section-title" style={{ margin: 0, flex: 1 }}>
                Connected sources · {playlists.length}
              </h3>
              <button className="primary-button" onClick={onAddSource}>
                <Icon name="plus" size={17} />Add source
              </button>
            </div>
            {!playlists.length && (
              <p className="muted-note">
                No sources yet. Add an M3U playlist file, a playlist URL, an extracted page, or a public free-to-air catalog.
              </p>
            )}
            {playlists.map((source) => (
              <div className="settings-source" key={source.id}>
                <div>
                  <h3>{source.name}</h3>
                  <p>
                    {source.source} · Added {new Date(source.created_at * 1000).toLocaleDateString()}
                  </p>
                  {confirmId === source.id && (
                    <p style={{ marginTop: 8 }}>Remove its channels and saved favorites? Original files stay untouched.</p>
                  )}
                </div>
                <div className="extract-buttons">
                  {confirmId === source.id ? (
                    <>
                      <button
                        className="danger-button"
                        disabled={saving}
                        onClick={async () => {
                          setSaving(true);
                          try {
                            await onDeleteSource(source.id);
                            setConfirmId(null);
                          } catch (failure) {
                            setError(errorMessage(failure));
                          } finally {
                            setSaving(false);
                          }
                        }}
                      >
                        Confirm removal
                      </button>
                      <button className="secondary-button" onClick={() => setConfirmId(null)}>
                        Cancel
                      </button>
                    </>
                  ) : (
                    <button className="danger-button" onClick={() => setConfirmId(source.id)}>
                      Remove
                    </button>
                  )}
                </div>
              </div>
            ))}
          </>
        )}

        {section === "files" && (
          <div className="settings-files">
            <p className="settings-intro">
              Browse your folders and play with the native engine. Resume position is remembered per file.
            </p>
            <LocalBrowser onPlayFile={onPlayFile} onPlaylistImported={onRefresh} />
          </div>
        )}

        {section === "mylist" && (
          <>
            <p className="settings-intro">Everything you saved with the + button, in one place.</p>
            {myList.length === 0 ? (
              <div className="empty-state">
                <div className="empty-symbol">
                  <Icon name="plus" size={30} />
                </div>
                <h2>Nothing saved yet.</h2>
                <p>Use the + on any title to add it to your list.</p>
                <button className="primary-button" onClick={onBrowseDiscovery}>
                  Browse Discovery
                </button>
              </div>
            ) : (
              <MediaShelf
                title="My list"
                subtitle="Saved titles."
                channels={myList}
                favorites={new Set(myList.map((channel) => channel.id))}
                onPlay={onPlayChannel}
                onDetails={onDetails}
                onFavorite={onToggleFavorite}
                compact={preferences.compactCards}
              />
            )}
          </>
        )}

        {section === "metadata" && (
          <>
            <h3 className="settings-section-title">Posters, genres and years</h3>
            <p className="settings-intro">
              TMDB supplies real posters, genres and release years for the films and series in your library, which is what
              the Genre and Year categories are built from. Live channels have no TMDB record, so their genre is inferred
              from the source group and they land under “Year unknown”.
            </p>

            <div className="form-group">
              <label htmlFor="poster-frame">Local video poster frame</label>
              <select id="poster-frame" value={posterFrame} onChange={(event) => void savePosterFrame(event.target.value)}>
                {posterFrameChoices.map((choice) => <option key={choice.seconds} value={choice.seconds}>{choice.label}</option>)}
              </select>
              <small>
                The frame shown as the poster for local video files. Posters are regenerated from the new frame the next time they are shown.
              </small>
            </div>

            <div className="form-group">
              <label htmlFor="poster-frame-path">Preview frames from a video</label>
              <div className="poster-frame-lookup">
                <input
                  id="poster-frame-path"
                  type="text"
                  spellCheck={false}
                  placeholder="Full path to a local video file"
                  value={framePath}
                  onChange={(event) => setFramePath(event.target.value)}
                  onKeyDown={(event) => { if (event.key === "Enter") void loadFrameCandidates(); }}
                />
                <button type="button" className="secondary-button" disabled={frameCandidatesBusy || !framePath.trim()} onClick={() => void loadFrameCandidates()}>
                  {frameCandidatesBusy ? "Loading…" : "Show frames"}
                </button>
              </div>
              {frameCandidates && (
                <div className="poster-frame-grid" role="group" aria-label="Poster frame options">
                  {frameCandidates.map((candidate) => {
                    const choice = posterFrameChoices.find((item) => Number(item.seconds) === candidate.seconds);
                    const selected = String(candidate.seconds) === posterFrame;
                    return (
                      <button
                        key={candidate.seconds}
                        type="button"
                        className={`poster-frame-option${selected ? " is-selected" : ""}`}
                        aria-pressed={selected}
                        disabled={!candidate.image}
                        onClick={() => void savePosterFrame(String(candidate.seconds))}
                      >
                        {candidate.image ? <img src={candidate.image} alt="" /> : <span className="poster-frame-empty">Past end of video</span>}
                        <span>{choice?.label ?? `${candidate.seconds} seconds`}</span>
                      </button>
                    );
                  })}
                </div>
              )}
              <small>Pick a frame to save it as the poster. Frames past the end of a short video are unavailable.</small>
            </div>

            <div className="form-group">
              <label htmlFor="tmdb-key">TMDB API key</label>
              <input
                id="tmdb-key"
                type="password"
                autoComplete="off"
                spellCheck={false}
                placeholder={tmdb?.configured ? "Saved on this device — enter a new value to replace it" : "Paste your TMDB v3 API key or v4 read access token"}
                value={tmdbKey}
                onChange={(event) => setTmdbKey(event.target.value)}
              />
              <small>
                {tmdb?.key_source === "env"
                  ? "Currently supplied by the LUMEN_TMDB_API_KEY environment variable, which takes precedence over anything saved here."
                  : tmdb?.key_source === "keychain"
                    ? "A key is saved in this device's OS keychain."
                    : tmdb?.key_source === "memory"
                      ? "A key is held in memory for this browser preview only. It is lost when the sidecar restarts."
                      : "Required before any title can be enriched."}{" "}
                Create one free at developer.themoviedb.org → Settings → API. The key is never written to the library
                database and is never sent anywhere but TMDB.
              </small>
            </div>

            <div className="settings-actions">
              <button className="primary-button" disabled={tmdbBusy || !tmdbKey.trim()} onClick={() => void saveTmdbKey()}>
                Save key
              </button>
              {tmdb?.configured && (
                <button className="secondary-button" disabled={tmdbBusy} onClick={() => void runEnrich(false)}>
                  {tmdbBusy ? "Working…" : "Enrich library"}
                </button>
              )}
              {tmdb?.configured && (
                <button className="secondary-button" disabled={tmdbBusy} onClick={() => void runEnrich(true)}>
                  Re-check all
                </button>
              )}
              {tmdb && tmdb.enriched > 0 && (
                <button className="secondary-button" disabled={tmdbBusy} onClick={() => void clearTmdbMetadata()}>
                  Clear metadata
                </button>
              )}
            </div>

            {tmdb && (
              <div className="settings-note">
                <strong>{tmdb.enriched} of {tmdb.total} titles carry TMDB metadata.</strong>
                <br />
                Every title is attempted once. Unmatched titles are remembered, so a run of “Enrich library” always continues
                where the last one stopped instead of re-querying your whole library.
              </div>
            )}

            {tmdbNotice && <p className="saved-badge" role="status">{tmdbNotice}</p>}

            <p className="tmdb-attribution">
              This product uses the TMDB API but is not endorsed or certified by TMDB.
            </p>
          </>
        )}

        {section === "playback" && (
          <>
            <p className="settings-intro">
              A great picture. Smooth playback. Tune the native engine for your hardware.
            </p>
            {loading ? (
              <p role="status">Loading playback settings…</p>
            ) : (
              <>
                <div className="form-group">
                  <label htmlFor="hwdec">Hardware acceleration</label>
                  <select id="hwdec" value={hwdec} onChange={(event) => setHwdec(event.target.value)}>
                    <option value="auto-safe">Automatic · safe (recommended)</option>
                    <option value="auto">Automatic · all supported decoders</option>
                    <option value="vaapi">VA-API · Intel / AMD</option>
                    <option value="nvdec">NVDEC · NVIDIA</option>
                    <option value="no">Software decoding</option>
                  </select>
                  <small>Use software decoding if the picture flickers or your driver is unstable.</small>
                </div>
                <div className="form-group">
                  <label htmlFor="cache">Stream buffering</label>
                  <select id="cache" value={cache} onChange={(event) => setCache(event.target.value)}>
                    <option value="yes">Enabled · smoother live streams</option>
                    <option value="auto">Automatic</option>
                    <option value="no">Disabled · lower latency</option>
                  </select>
                  <small>Buffering helps HLS and IPTV streams on slower connections.</small>
                </div>
                <div className="form-group">
                  <label htmlFor="default-volume">Starting volume · {draft.defaultVolume}%</label>
                  <input
                    id="default-volume"
                    type="range"
                    min="0"
                    max="100"
                    value={draft.defaultVolume}
                    onChange={(event) => update("defaultVolume", Number(event.target.value))}
                    style={{ width: "100%" }}
                  />
                </div>
                <div className="form-group">
                  <label htmlFor="mpv-args">Advanced engine arguments</label>
                  <input
                    id="mpv-args"
                    type="text"
                    placeholder="--demuxer-max-bytes=50MiB"
                    value={extraArgs}
                    onChange={(event) => setExtraArgs(event.target.value)}
                  />
                  <small>Space-separated arguments. Avoid quoted values containing spaces; the engine does not parse shell quoting.</small>
                </div>
              </>
            )}
            <div className="settings-note">
              <strong>Native playback, no compromise.</strong>
              <br />
              On the desktop app video opens in a separate mpv window. Hardware settings apply when that engine restarts. Browser preview uses an embedded player, so hardware decoding options apply only to native playback.
            </div>
          </>
        )}

        {section === "appearance" && (
          <>
            <fieldset className="theme-fieldset"><legend>Choose your theme</legend><p>A look for every mood. Preview instantly; save to keep it on this device.</p><div className="theme-grid">{themes.map((theme) => <label className="theme-option" key={theme.id} style={{ "--swatch-accent": theme.accent, "--swatch-surface": theme.surface } as CSSProperties}>
              <input type="radio" name="theme" value={theme.id} checked={draft.theme === theme.id} onChange={() => update("theme", theme.id)} aria-label={`${theme.name} theme`} />
              <span className="theme-mini" style={{ background: theme.id === "system" ? "linear-gradient(110deg,#f4f5f7 50%,#141414 50%)" : theme.background }} aria-hidden="true"><span className="theme-mini-bars" /><span className="theme-mini-grid"><i /><i /><i /></span></span>
              <span className="theme-option-name">{theme.name}{draft.theme === theme.id ? <Icon name="check" size={15} /> : theme.id === "system" ? <Icon name="monitor" size={14} /> : null}</span><span className="theme-option-desc">{theme.description}</span>
            </label>)}</div></fieldset>
            <p className="settings-preferences-label">BROWSING & ACCESSIBILITY</p>
            {toggle("compactCards", "Compact shelves", "Fit more titles into every row.")}
            {toggle("reduceMotion", "Reduce motion", "Disable animations and use instant shelf scrolling.")}
            {toggle("personalizedRows", "Personalized discovery", "Recommend titles from saved favorites and recently watched categories. No data leaves your device.")}
            {toggle("hideUnavailable", "Hide unavailable streams", "Hide titles whose most recent stream check failed. Unchecked streams remain visible.")}
          </>
        )}

        {section === "shortcuts" && (
          <>
            <h3 className="settings-section-title">Less clicking. More watching.</h3>
            <p>
              Playback shortcuts work while media is active. They stay out of the way while typing, using buttons, or editing a dialog.
            </p>
            {toggle("keyboardShortcuts", "Playback keyboard shortcuts", "Control playback from the main Lumen window.")}
            <div className="shortcut-grid">
              {[
                ["Play / pause", "Space"],
                ["Seek 10 seconds", "← / →"],
                ["Mute / unmute", "M"],
                ["Volume ±5%", "9 / 0"],
                ["App fullscreen", "F"],
                ["Close dialog", "Esc"],
              ].map(([label, key]) => (
                <div key={label}>
                  <span>{label}</span>
                  <kbd>{key}</kbd>
                </div>
              ))}
            </div>
          </>
        )}

        {(section === "playback" || section === "appearance" || section === "shortcuts") && (
          <div className="settings-actions">
            <span className={`settings-save-state ${draftDirty ? "dirty" : ""}`}><Icon name={draftDirty ? "info" : "check"} size={14} />{draftDirty ? "Unsaved changes" : "Up to date"}</span>
            <button className="secondary-button" disabled={saving || !draftDirty} onClick={() => { if (section === "playback") { setHwdec(savedEngine.hwdec); setCache(savedEngine.cache); setExtraArgs(savedEngine.extraArgs); setDraft((previous) => ({ ...previous, defaultVolume: preferences.defaultVolume })); } else if (section === "appearance") { setDraft((previous) => ({ ...previous, theme: preferences.theme, compactCards: preferences.compactCards, reduceMotion: preferences.reduceMotion, personalizedRows: preferences.personalizedRows, hideUnavailable: preferences.hideUnavailable })); } else setDraft((previous) => ({ ...previous, keyboardShortcuts: preferences.keyboardShortcuts })); setNotice(null); }}>Discard</button>
            <button onClick={() => void save()} disabled={saving || !draftDirty || (section === "playback" && (loading || Boolean(engineError)))}>
              {saving ? "Saving…" : "Save changes"}
            </button>
            <button
              className="secondary-button"
              onClick={() => {
                if (section === "playback") { setHwdec("auto-safe"); setCache("yes"); setExtraArgs(""); setDraft((previous) => ({ ...previous, defaultVolume: defaultPreferences.defaultVolume })); }
                else if (section === "appearance") setDraft((previous) => ({ ...previous, theme: defaultPreferences.theme, compactCards: defaultPreferences.compactCards, reduceMotion: defaultPreferences.reduceMotion, personalizedRows: defaultPreferences.personalizedRows, hideUnavailable: defaultPreferences.hideUnavailable }));
                else setDraft((previous) => ({ ...previous, keyboardShortcuts: defaultPreferences.keyboardShortcuts }));
                setNotice("Defaults restored in the form. Save changes to apply.");
              }}
            >
              Restore defaults
            </button>
          </div>
        )}
        {notice && (
          <p className="saved-badge" role="status" style={{ margin: "18px 0 0" }}>
            {notice}
          </p>
        )}
      </section>
    </div>
  );
}

import { useState, useEffect, useRef } from "react";
import type { CSSProperties } from "react";
import { getSettings, setSetting, setTmdbApiKey, tmdbStatus, enrichTmdb, clearTmdbEnrichment, clearLocalThumbnailCache, localPosterCandidates, extractorStatus, updateExtractor, errorMessage } from "../lib/tauri";
import type { Channel, ExtractorStatus, Playlist, PosterCandidate, Profile, RefreshStats, TmdbStatus } from "../lib/types";
import { defaultPreferences, themes, applyTheme } from "../lib/discovery";
import type { Preferences } from "../lib/discovery";
import { POSTER_FRAME_OPTIONS } from "../lib/local";
import { ProfileSettings } from "./ProfileSettings";
import { SyncSettings } from "./SyncSettings";
import { NetworkPrivacySettings } from "./NetworkPrivacySettings";
import { PlayerPrefsSettings } from "./PlayerPrefsSettings";
import { OnlineSubtitlesSettings } from "./OnlineSubtitlesSettings";
import { LanguageSetting } from "./LanguageSetting";
import { AboutLumen } from "./AboutLumen";
import { EpisodeAlertsSetting } from "./EpisodeAlertsSetting";
import { EmbeddedPlayerToggle } from "./EmbeddedPlayerToggle";
import { LanSourcesToggle } from "./LanSourcesToggle";
import { t } from "../i18n/index.ts";
import { MediaShelf } from "./MediaShelf";
import { Icon } from "./Icon";
import type { IconName } from "./Icon";

export type SettingsSection = "account" | "sources" | "mylist" | "metadata" | "playback" | "appearance" | "shortcuts" | "about";

interface SettingsPageProps {
  preferences: Preferences;
  onPreferencesChange: (preferences: Preferences) => void;
  playlists: Playlist[];
  myList: Channel[];
  onAddSource: () => void;
  onDeleteSource: (id: number) => Promise<void>;
  onRefreshSource: (id: number) => Promise<RefreshStats>;
  onRefresh: () => void;
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

const posterFrameChoices = POSTER_FRAME_OPTIONS.map((option) => ({ seconds: String(option.seconds), label: option.label }));

type SettingsGroup = "you" | "library" | "preferences" | "app";

const groupLabels: Record<SettingsGroup, () => string> = {
  you: () => t("settings.group.you"),
  library: () => t("settings.group.library"),
  preferences: () => t("settings.group.preferences"),
  app: () => t("settings.group.app"),
};

function buildSections(): { id: SettingsSection; label: string; description: string; icon: IconName; group: SettingsGroup }[] {
  return [
    { id: "account", label: t("settings.section.account.label"), description: t("settings.section.account.description"), icon: "profile", group: "you" },
    { id: "sources", label: t("settings.section.sources.label"), description: t("settings.section.sources.description"), icon: "grid", group: "library" },
    { id: "mylist", label: t("settings.section.mylist.label"), description: t("settings.section.mylist.description"), icon: "bookmark", group: "library" },
    { id: "metadata", label: t("settings.section.metadata.label"), description: t("settings.section.metadata.description"), icon: "film", group: "library" },
    { id: "playback", label: t("settings.section.playback.label"), description: t("settings.section.playback.description"), icon: "play", group: "preferences" },
    { id: "appearance", label: t("settings.section.appearance.label"), description: t("settings.section.appearance.description"), icon: "palette", group: "preferences" },
    { id: "shortcuts", label: t("settings.section.shortcuts.label"), description: t("settings.section.shortcuts.description"), icon: "keyboard", group: "preferences" },
    { id: "about", label: t("settings.section.about.label"), description: t("settings.section.about.description"), icon: "info", group: "app" },
  ];
}

export function SettingsPage({
  preferences,
  onPreferencesChange,
  playlists,
  myList,
  onAddSource,
  onDeleteSource,
  onRefreshSource,
  onRefresh,
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
  const sections = buildSections();
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
  const [refreshingId, setRefreshingId] = useState<number | null>(null);
  const [loadRevision, setLoadRevision] = useState(0);
  const [engineError, setEngineError] = useState<string | null>(null);
  const [savedEngine, setSavedEngine] = useState({ hwdec: "auto-safe", cache: "yes", extraArgs: "" });
  const appearanceKeys = ["theme", "compactCards", "reduceMotion", "personalizedRows", "hideUnavailable", "showAdult"] as const;
  const draftDirty = section === "appearance" ? appearanceKeys.some((key) => draft[key] !== preferences[key])
    : section === "shortcuts" ? draft.keyboardShortcuts !== preferences.keyboardShortcuts
    : section === "playback" ? hwdec !== savedEngine.hwdec || cache !== savedEngine.cache || extraArgs.trim() !== savedEngine.extraArgs || draft.defaultVolume !== preferences.defaultVolume : false;

  // --- TMDB metadata & Extractor -------------------------------------------
  const [tmdbKey, setTmdbKey] = useState("");
  const [tmdb, setTmdb] = useState<TmdbStatus | null>(null);
  const [tmdbBusy, setTmdbBusy] = useState(false);
  const [tmdbNotice, setTmdbNotice] = useState<string | null>(null);
  const [extractor, setExtractor] = useState<ExtractorStatus | null>(null);
  const [extractorBusy, setExtractorBusy] = useState(false);
  const [extractorNotice, setExtractorNotice] = useState<string | null>(null);
  const [posterFrame, setPosterFrame] = useState("2");
  const [framePath, setFramePath] = useState("");
  const [frameCandidates, setFrameCandidates] = useState<PosterCandidate[] | null>(null);
  const [frameCandidatesBusy, setFrameCandidatesBusy] = useState(false);

  useEffect(() => {
    if (section !== "sources") return;
    let active = true;
    extractorStatus()
      .then((status) => { if (active) setExtractor(status); })
      .catch(() => {});
    return () => { active = false; };
  }, [section]);

  const handleUpdateExtractor = async () => {
    setExtractorBusy(true);
    setError(null);
    setExtractorNotice(null);
    try {
      const status = await updateExtractor();
      setExtractor(status);
      setExtractorNotice(t("settings.extractor.updated", { version: status.version || status.pinnedVersion }));
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setExtractorBusy(false);
    }
  };

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
      // Discovery loads TMDB browse titles only when a key exists, so it must reload now.
      onRefresh();
      setTmdbNotice(status.configured ? t("settings.metadata.keySaved") : t("settings.metadata.keyCleared"));
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
      setTmdbNotice(t("settings.metadata.checked", { checked: result.matched + result.skipped, matched: result.matched, remaining: result.remaining }));
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
      setTmdbNotice(t("settings.metadata.cleared", { count: cleared }));
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

  // The shared library hook (useChannels) already reloads its own playlists/channels and
  // surfaces the "added/updated/removed" result as the app's global notice toast on success —
  // this only needs to track its own per-row busy state and the error case, which isn't.
  const refreshSource = async (id: number) => {
    setRefreshingId(id);
    setError(null);
    try {
      await onRefreshSource(id);
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setRefreshingId(null);
    }
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
        next = { ...next, theme: draft.theme, compactCards: draft.compactCards, reduceMotion: draft.reduceMotion, personalizedRows: draft.personalizedRows, hideUnavailable: draft.hideUnavailable, showAdult: draft.showAdult };
      } else if (section === "shortcuts") next.keyboardShortcuts = draft.keyboardShortcuts;
      localStorage.setItem("lumen.preferences", JSON.stringify(next));
      onPreferencesChange(next);
      setNotice(section === "playback" ? t("settings.save.savedPlayback") : t("settings.save.savedPreferences"));
    } catch (failure) {
      setError(t("settings.save.failed", { reason: errorMessage(failure) }));
    } finally {
      setSaving(false);
    }
  };

  const toggle = (
    key: "compactCards" | "reduceMotion" | "hideUnavailable" | "showAdult" | "personalizedRows" | "keyboardShortcuts",
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
      <aside className="settings-rail" aria-label={t("settings.sectionsLabel")}>
        <p className="eyebrow">{t("settings.eyebrow")}</p>
        <h1 className="settings-rail-title">{t("settings.title")}</h1>
        <p className="settings-rail-intro">{t("settings.intro")}</p>
        <nav>
          {sections.map((item, index) => (
            <div key={item.id}>{(index === 0 || sections[index - 1].group !== item.group) && <p className="settings-nav-group">{groupLabels[item.group]()}</p>}<button
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
            <Icon name="plus" size={17} />{t("settings.addSource")}
          </button>
          <button className="secondary-button" onClick={onRefresh}>
            <Icon name="refresh" size={15} />{t("settings.refreshLibrary")}
          </button>
        </div>
        <div className="settings-device"><Icon name="monitor" size={17} /><div><strong>{t("settings.device.title")}</strong><span>{t("settings.device.note")}</span></div></div>
      </aside>

      <section className="settings-panel" role="region" aria-label={active.label}>
        <header className="settings-panel-head">
          <div className="settings-breadcrumb">{t("settings.title")} <Icon name="chevron" size={12} /> {active.label}</div>
          <div className="settings-title-row"><span className="settings-title-icon"><Icon name={active.icon} size={23} /></span><div><h2>{active.label}</h2><p>{active.description}</p></div></div>
        </header>

        {error && (
          <div className="dialog-error" role="alert">
            {error}{" "}
            <button className="icon-button" onClick={() => setLoadRevision((value) => value + 1)} aria-label={t("settings.reload")}>
              <Icon name="refresh" size={16} />
            </button>
          </div>
        )}
        {section === "playback" && engineError && <div className="dialog-error" role="alert">{t("settings.playbackLoadFailed", { reason: engineError })}<button className="icon-button" aria-label={t("settings.playbackRetry")} onClick={() => setLoadRevision((value) => value + 1)}><Icon name="refresh" size={16} /></button></div>}
        {(section === "sources" || section === "mylist") && loadError && <div className="dialog-error" role="alert">{loadError}</div>}

        {section === "account" && (
          <>
            <SyncSettings />
            <NetworkPrivacySettings />
            <h3 className="settings-section-title">{t("settings.account.whoTitle")}</h3>
            <p>{t("settings.account.whoNote")}</p>
            <ProfileSettings
              profiles={profiles}
              activeProfileId={activeProfileId}
              onProfilesChanged={onProfilesChanged}
            />
          </>
        )}

        {section === "sources" && (
          <>
            <LanSourcesToggle />
            <div className="setting-row">
              <div>
                <label>{t("settings.extractor.label")}</label>
                <p>
                  {extractor?.available
                    ? extractor.version
                      ? t("settings.extractor.activeWithVersion", { source: extractor.source, version: extractor.version, pinned: extractor.pinnedVersion })
                      : t("settings.extractor.active", { source: extractor.source, pinned: extractor.pinnedVersion })
                    : t("settings.extractor.notInstalled", { pinned: extractor?.pinnedVersion || "2026.02.04" })}
                </p>
                {extractorNotice && <p className="saved-badge" role="status">{extractorNotice}</p>}
              </div>
              <button className="secondary-button" disabled={extractorBusy} onClick={() => void handleUpdateExtractor()}>
                {extractorBusy ? t("settings.extractor.updating") : t("settings.extractor.update")}
              </button>
            </div>
            <div className="settings-actions settings-actions-top">
              <h3 className="settings-section-title" style={{ margin: 0, flex: 1 }}>
                {t("settings.sources.connected", { count: playlists.length })}
              </h3>
              <button className="primary-button" onClick={onAddSource}>
                <Icon name="plus" size={17} />{t("settings.addSource")}
              </button>
            </div>
            {!playlists.length && (
              <p className="muted-note">
                {t("settings.sources.empty")}
              </p>
            )}
            {playlists.map((source) => (
              <div className="settings-source" key={source.id}>
                <div>
                  <h3>{source.name}</h3>
                  <p>
                    {t("settings.sources.added", { source: source.source, date: new Date(source.created_at * 1000).toLocaleDateString() })}
                  </p>
                  {confirmId === source.id && (
                    <p style={{ marginTop: 8 }}>{t("settings.sources.removeNote")}</p>
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
                        {t("settings.sources.confirmRemoval")}
                      </button>
                      <button className="secondary-button" onClick={() => setConfirmId(null)}>
                        {t("common.cancel")}
                      </button>
                    </>
                  ) : (
                    <>
                      {source.source === "url" && (
                        <button
                          className="secondary-button"
                          disabled={refreshingId === source.id}
                          onClick={() => void refreshSource(source.id)}
                        >
                          {refreshingId === source.id ? t("settings.sources.refreshing") : t("settings.sources.refresh")}
                        </button>
                      )}
                      <button className="danger-button" onClick={() => setConfirmId(source.id)}>
                        {t("settings.sources.remove")}
                      </button>
                    </>
                  )}
                </div>
              </div>
            ))}
          </>
        )}

        {section === "mylist" && (
          <>
            <p className="settings-intro">{t("settings.mylist.intro")}</p>
            {myList.length === 0 ? (
              <div className="empty-state">
                <div className="empty-symbol">
                  <Icon name="plus" size={30} />
                </div>
                <h2>{t("settings.mylist.emptyTitle")}</h2>
                <p>{t("settings.mylist.emptyHint")}</p>
                <button className="primary-button" onClick={onBrowseDiscovery}>
                  {t("settings.mylist.browse")}
                </button>
              </div>
            ) : (
              <MediaShelf
                title={t("settings.mylist.shelfTitle")}
                subtitle={t("settings.mylist.shelfSubtitle")}
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
            <h3 className="settings-section-title">{t("settings.metadata.title")}</h3>
            <p className="settings-intro">{t("settings.metadata.intro")}</p>

            <div className="form-group">
              <label htmlFor="poster-frame">{t("settings.metadata.posterFrame")}</label>
              <select id="poster-frame" value={posterFrame} onChange={(event) => void savePosterFrame(event.target.value)}>
                {posterFrameChoices.map((choice) => <option key={choice.seconds} value={choice.seconds}>{choice.label}</option>)}
              </select>
              <small>{t("settings.metadata.posterFrameNote")}</small>
            </div>

            <div className="form-group">
              <label htmlFor="poster-frame-path">{t("settings.metadata.previewFrames")}</label>
              <div className="poster-frame-lookup">
                <input
                  id="poster-frame-path"
                  type="text"
                  spellCheck={false}
                  placeholder={t("settings.metadata.framePathPlaceholder")}
                  value={framePath}
                  onChange={(event) => setFramePath(event.target.value)}
                  onKeyDown={(event) => { if (event.key === "Enter") void loadFrameCandidates(); }}
                />
                <button type="button" className="secondary-button" disabled={frameCandidatesBusy || !framePath.trim()} onClick={() => void loadFrameCandidates()}>
                  {frameCandidatesBusy ? t("common.loading") : t("settings.metadata.showFrames")}
                </button>
              </div>
              {frameCandidates && (
                <div className="poster-frame-grid" role="group" aria-label={t("settings.metadata.frameOptions")}>
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
                        {candidate.image ? <img src={candidate.image} alt="" /> : <span className="poster-frame-empty">{t("settings.metadata.pastEnd")}</span>}
                        <span>{choice?.label ?? t("settings.metadata.frameSeconds", { seconds: candidate.seconds })}</span>
                      </button>
                    );
                  })}
                </div>
              )}
              <small>{t("settings.metadata.framePickNote")}</small>
            </div>

            <div className="form-group">
              <label htmlFor="tmdb-key">{t("settings.metadata.keyLabel")}</label>
              <input
                id="tmdb-key"
                type="password"
                autoComplete="off"
                spellCheck={false}
                placeholder={tmdb?.configured ? t("settings.metadata.keyPlaceholderSaved") : t("settings.metadata.keyPlaceholderNew")}
                value={tmdbKey}
                onChange={(event) => setTmdbKey(event.target.value)}
              />
              <small>
                {tmdb?.key_source === "env"
                  ? t("settings.metadata.keyNoteEnv")
                  : tmdb?.key_source === "keychain"
                    ? t("settings.metadata.keyNoteKeychain")
                    : t("settings.metadata.keyNoteNone")}
              </small>
            </div>

            <div className="settings-actions">
              <button className="primary-button" disabled={tmdbBusy || !tmdbKey.trim()} onClick={() => void saveTmdbKey()}>
                {t("settings.metadata.saveKey")}
              </button>
              {tmdb?.configured && (
                <button className="secondary-button" disabled={tmdbBusy} onClick={() => void runEnrich(false)}>
                  {tmdbBusy ? t("sync.working") : t("settings.metadata.enrich")}
                </button>
              )}
              {tmdb?.configured && (
                <button className="secondary-button" disabled={tmdbBusy} onClick={() => void runEnrich(true)}>
                  {t("settings.metadata.recheck")}
                </button>
              )}
              {tmdb && tmdb.enriched > 0 && (
                <button className="secondary-button" disabled={tmdbBusy} onClick={() => void clearTmdbMetadata()}>
                  {t("settings.metadata.clear")}
                </button>
              )}
            </div>

            {tmdb && (
              <div className="settings-note">
                <strong>{t("settings.metadata.coverage", { enriched: tmdb.enriched, total: tmdb.total })}</strong>
                <br />
                {t("settings.metadata.coverageNote")}
              </div>
            )}

            {tmdbNotice && <p className="saved-badge" role="status">{tmdbNotice}</p>}

            <EpisodeAlertsSetting />

            <p className="tmdb-attribution">
              {t("settings.metadata.attribution")}
            </p>
          </>
        )}

        {section === "playback" && (
          <>
            <p className="settings-intro">{t("settings.playback.intro")}</p>
            <PlayerPrefsSettings />
            <OnlineSubtitlesSettings />
            <EmbeddedPlayerToggle />
            {loading ? (
              <p role="status">{t("settings.playback.loading")}</p>
            ) : (
              <>
                <div className="form-group">
                  <label htmlFor="hwdec">{t("settings.playback.hardwareDecoding")}</label>
                  <select id="hwdec" value={hwdec} onChange={(event) => setHwdec(event.target.value)}>
                    <option value="auto-safe">{t("settings.playback.hwdecAutoSafe")}</option>
                    <option value="auto">{t("settings.playback.hwdecAuto")}</option>
                    <option value="vaapi">{t("settings.playback.hwdecVaapi")}</option>
                    <option value="nvdec">{t("settings.playback.hwdecNvdec")}</option>
                    <option value="no">{t("settings.playback.hwdecNo")}</option>
                  </select>
                  <small>{t("settings.playback.hwdecNote")}</small>
                </div>
                <div className="form-group">
                  <label htmlFor="cache">{t("settings.playback.buffering")}</label>
                  <select id="cache" value={cache} onChange={(event) => setCache(event.target.value)}>
                    <option value="yes">{t("settings.playback.cacheYes")}</option>
                    <option value="auto">{t("settings.playback.cacheAuto")}</option>
                    <option value="no">{t("settings.playback.cacheNo")}</option>
                  </select>
                  <small>{t("settings.playback.bufferingNote")}</small>
                </div>
                <div className="form-group">
                  <label htmlFor="default-volume">{t("settings.playback.startingVolume", { volume: draft.defaultVolume })}</label>
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
                  <label htmlFor="mpv-args">{t("settings.playback.engineArgs")}</label>
                  <input
                    id="mpv-args"
                    type="text"
                    placeholder={t("settings.playback.engineArgsPlaceholder")}
                    value={extraArgs}
                    onChange={(event) => setExtraArgs(event.target.value)}
                  />
                  <small>{t("settings.playback.engineArgsNote")}</small>
                </div>
              </>
            )}
            <div className="settings-note">
              <strong>{t("settings.playback.nativeTitle")}</strong>
              <br />
              {t("settings.playback.nativeNote")}
            </div>
          </>
        )}

        {section === "appearance" && (
          <>
            <fieldset className="theme-fieldset"><legend>{t("settings.appearance.chooseTheme")}</legend><p>{t("settings.appearance.themeIntro")}</p><div className="theme-grid">{themes.map((theme) => <label className="theme-option" key={theme.id} style={{ "--swatch-accent": theme.accent, "--swatch-surface": theme.surface } as CSSProperties}>
              <input type="radio" name="theme" value={theme.id} checked={draft.theme === theme.id} onChange={() => update("theme", theme.id)} aria-label={t("settings.appearance.themeAria", { name: theme.name })} />
              <span className="theme-mini" style={{ background: theme.id === "system" ? "linear-gradient(110deg,#f4f5f7 50%,#141414 50%)" : theme.background }} aria-hidden="true"><span className="theme-mini-bars" /><span className="theme-mini-grid"><i /><i /><i /></span></span>
              <span className="theme-option-name">{theme.name}{draft.theme === theme.id ? <Icon name="check" size={15} /> : theme.id === "system" ? <Icon name="monitor" size={14} /> : null}</span><span className="theme-option-desc">{theme.description}</span>
            </label>)}</div></fieldset>
            <LanguageSetting />
            <p className="settings-preferences-label">{t("settings.appearance.browsingLabel")}</p>
            {toggle("compactCards", t("settings.appearance.compactLabel"), t("settings.appearance.compactDescription"))}
            {toggle("reduceMotion", t("settings.appearance.reduceMotionLabel"), t("settings.appearance.reduceMotionDescription"))}
            {toggle("personalizedRows", t("settings.appearance.personalizedLabel"), t("settings.appearance.personalizedDescription"))}
            {toggle("hideUnavailable", t("settings.appearance.hideUnavailableLabel"), t("settings.appearance.hideUnavailableDescription"))}
            {toggle("showAdult", t("settings.appearance.showAdultLabel"), t("settings.appearance.showAdultDescription"))}
          </>
        )}

        {section === "shortcuts" && (
          <>
            <h3 className="settings-section-title">{t("settings.shortcuts.title")}</h3>
            <p>{t("settings.shortcuts.intro")}</p>
            {toggle("keyboardShortcuts", t("settings.shortcuts.toggleLabel"), t("settings.shortcuts.toggleDescription"))}
            <div className="shortcut-grid">
              {[
                [t("settings.shortcuts.playPause"), t("settings.shortcuts.keySpace")],
                [t("settings.shortcuts.seek"), "← / →"],
                [t("settings.shortcuts.mute"), "M"],
                [t("settings.shortcuts.volume"), "9 / 0"],
                [t("settings.shortcuts.fullscreen"), "F"],
                [t("settings.shortcuts.closeDialog"), t("settings.shortcuts.keyEsc")],
              ].map(([label, key]) => (
                <div key={label}>
                  <span>{label}</span>
                  <kbd>{key}</kbd>
                </div>
              ))}
            </div>
          </>
        )}

        {section === "about" && <AboutLumen />}

        {(section === "playback" || section === "appearance" || section === "shortcuts") && (
          <div className="settings-actions">
            <span className={`settings-save-state ${draftDirty ? "dirty" : ""}`}><Icon name={draftDirty ? "info" : "check"} size={14} />{draftDirty ? t("settings.save.dirty") : t("settings.save.clean")}</span>
            <button className="secondary-button" disabled={saving || !draftDirty} onClick={() => { if (section === "playback") { setHwdec(savedEngine.hwdec); setCache(savedEngine.cache); setExtraArgs(savedEngine.extraArgs); setDraft((previous) => ({ ...previous, defaultVolume: preferences.defaultVolume })); } else if (section === "appearance") { setDraft((previous) => ({ ...previous, theme: preferences.theme, compactCards: preferences.compactCards, reduceMotion: preferences.reduceMotion, personalizedRows: preferences.personalizedRows, hideUnavailable: preferences.hideUnavailable, showAdult: preferences.showAdult })); } else setDraft((previous) => ({ ...previous, keyboardShortcuts: preferences.keyboardShortcuts })); setNotice(null); }}>{t("settings.save.discard")}</button>
            <button onClick={() => void save()} disabled={saving || !draftDirty || (section === "playback" && (loading || Boolean(engineError)))}>
              {saving ? t("settings.save.saving") : t("settings.save.save")}
            </button>
            <button
              className="secondary-button"
              onClick={() => {
                if (section === "playback") { setHwdec("auto-safe"); setCache("yes"); setExtraArgs(""); setDraft((previous) => ({ ...previous, defaultVolume: defaultPreferences.defaultVolume })); }
                else if (section === "appearance") setDraft((previous) => ({ ...previous, theme: defaultPreferences.theme, compactCards: defaultPreferences.compactCards, reduceMotion: defaultPreferences.reduceMotion, personalizedRows: defaultPreferences.personalizedRows, hideUnavailable: defaultPreferences.hideUnavailable, showAdult: defaultPreferences.showAdult }));
                else setDraft((previous) => ({ ...previous, keyboardShortcuts: defaultPreferences.keyboardShortcuts }));
                setNotice(t("settings.save.restored"));
              }}
            >
              {t("settings.save.restore")}
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

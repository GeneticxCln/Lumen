import { useState, useEffect, useMemo, useCallback, useRef } from "react";
import { Transport } from "./components/Transport";
import { PlayerOverlay } from "./components/PlayerOverlay";
import { PLAYER_SEEK_STEP_SECONDS, PLAYER_VOLUME_STEP, playerShortcutFor } from "./lib/playerKeys";
import type { PlayerShortcut } from "./lib/playerKeys";
import { SourceDialog } from "./components/SourceDialog";
import { SettingsPage } from "./components/SettingsPage";
import { ProfileGate } from "./components/ProfileGate";
import type { SettingsSection } from "./components/SettingsPage";
import { Modal } from "./components/Modal";
import { Icon } from "./components/Icon";
import type { IconName } from "./components/Icon";
import { ChannelArtwork } from "./components/MediaShelf";
import { FacetBrowser } from "./components/FacetBrowser";
import { DiscoveryPage } from "./components/DiscoveryPage";
import { LocalDevicePage } from "./components/LocalDevicePage";
import { SearchEnginePage } from "./components/SearchEnginePage";
import { usePlayer } from "./hooks/usePlayer";
import { useChannels } from "./hooks/useChannels";
import { activeProfile, boardNewChannelIds, browseTmdb, errorMessage, getHistory, listProfiles, setPlayerFullscreen, tmdbStatus } from "./lib/tauri";
import type { BrowseTitle } from "./lib/types";
import type { Channel, HistoryEntry, Profile } from "./lib/types";
import { readPreferences, recommendChannels, catalogShelves, contentKind, applyTheme, buildFacets, matchesFacets, facetCount, EMPTY_FACETS } from "./lib/discovery";
import { isLocalPath } from "./lib/local";
import { nextEpisode } from "./lib/episodes";
import { buildBoard } from "./lib/board";
import type { FacetSelection, Preferences } from "./lib/discovery";
import "./App.css";

type Page = "discovery" | "search" | "movies" | "series" | "live" | "local" | "settings";

// The top bar is exactly these five destinations: Discovery, Movies, Series and Live TV are your
// own sources, and Local Device is the media on this computer. Settings is deliberately absent — the header controls (My
// List and the profile button) and the footer already open it, so an entry would only duplicate
// them. Search is not a destination either: it lives in the top bar on every page.
type NavPage = Exclude<Page, "settings">;

const pages: { id: NavPage; label: string; icon: IconName }[] = [
  { id: "discovery", label: "Discovery", icon: "compass" },
  { id: "movies", label: "Movies", icon: "film" },
  { id: "series", label: "Series", icon: "series" },
  { id: "live", label: "Live TV", icon: "tv" },
  { id: "local", label: "Local Device", icon: "monitor" },
];

const headings: Record<Exclude<Page, "discovery" | "search" | "local" | "settings">, { title: string; blurb: string }> = {
  movies: { title: "Movies.", blurb: "Films and on-demand titles from your own sources." },
  series: { title: "Series.", blurb: "Shows and episodic titles from your own sources." },
  live: { title: "Live. Right now.", blurb: "Channels that are broadcasting as you browse." },
};

function App() {
  const [page, setPage] = useState<Page>("discovery");
  const [settingsSection, setSettingsSection] = useState<SettingsSection>("sources");
  const [showSource, setShowSource] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const [preferences, setPreferences] = useState<Preferences>(readPreferences);
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [historyError, setHistoryError] = useState<string | null>(null);
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [activeProfileId, setActiveProfileId] = useState<number | null>(null);
  const [locked, setLocked] = useState(false);
  const [profileError, setProfileError] = useState<string | null>(null);
  const lockEvaluated = useRef(false);
  const [detailId, setDetailId] = useState<number | null>(null);
  const [facetSelection, setFacetSelection] = useState<FacetSelection>(EMPTY_FACETS);
  const [playerOpen, setPlayerOpen] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  const searchRef = useRef<HTMLInputElement | null>(null);
  const library = useChannels(refresh);
  const [browseTitles, setBrowseTitles] = useState<BrowseTitle[]>([]);
  const player = usePlayer({ onNotice: (message) => setNotice(message) });
  const details = library.allChannels.find((channel) => channel.id === detailId) || null;
  const currentProfile = profiles.find((profile) => profile.id === activeProfileId) || null;
  const setDetails = (channel: Channel | null) => setDetailId(channel?.id ?? null);

  const loadProfiles = useCallback(async () => {
    try {
      const [list, active] = await Promise.all([listProfiles(), activeProfile()]);
      setProfiles(list);
      setActiveProfileId(active.id);
      setProfileError(null);
      // Lock once per session, on the first read: refreshing the list must not re-lock a
      // screen the user has already unlocked.
      if (!lockEvaluated.current) {
        lockEvaluated.current = true;
        setLocked(active.has_pin && list.length > 0);
      }
    } catch (error) {
      // The browser development bridge has no profile API. Say so and fail open rather than
      // hiding the library behind a lock that could never be satisfied.
      lockEvaluated.current = true;
      setProfiles([]);
      setActiveProfileId(null);
      setLocked(false);
      setProfileError(errorMessage(error));
    }
  }, []);

  useEffect(() => { void loadProfiles(); }, [loadProfiles]);

  // A profile switch (or an unlock) changes whose history and My List are on screen.
  const handleProfilesChanged = async () => {
    setLocked(false);
    await loadProfiles();
    setRefresh((value) => value + 1);
  };

  useEffect(() => {
    let active = true;
    getHistory(200).then((entries) => { if (active) { setHistory(entries); setHistoryError(null); } }).catch((error) => { if (active) setHistoryError(errorMessage(error)); });
    return () => { active = false; };
  }, [refresh, player.currentUrl, activeProfileId]);

  // TMDB's trending and top-rated titles appear once a key is configured. Without one, or if
  // TMDB is unreachable, Discovery simply has no browse shelf; the library still works.
  useEffect(() => {
    let active = true;
    tmdbStatus()
      .then(async (status) => {
        const { titles } = status.configured ? await browseTmdb() : { titles: [] };
        if (active) setBrowseTitles(titles);
      })
      .catch(() => { if (active) setBrowseTitles([]); });
    return () => { active = false; };
  }, [refresh]);

  useEffect(() => {
    document.documentElement.classList.toggle("reduce-motion", preferences.reduceMotion);
  }, [preferences.reduceMotion]);

  useEffect(() => {
    applyTheme(preferences.theme);
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const update = () => { if (preferences.theme === "system") applyTheme("system"); };
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, [preferences.theme]);

  async function run(action: () => Promise<unknown>) {
    setNotice(null);
    try { await action(); } catch (error) { setNotice(errorMessage(error)); }
  }

  async function play(channel: Channel) {
    if (busy) return;
    setBusy(true);
    await run(async () => { await player.playChannel(channel, preferences.defaultVolume); setPlayerOpen(true); setDetails(null); });
    setBusy(false);
  }

  async function playFile(path: string, name?: string, headers?: Record<string, string>, audioUrl?: string, channelId?: number) {
    try {
      await player.playMedia(path, name, headers, preferences.defaultVolume, audioUrl, channelId);
      setPlayerOpen(true);
    } catch (error) {
      setNotice(errorMessage(error));
      throw error;
    }
  }

  // Fullscreen drives the mpv window.
  const [isFullscreen, setIsFullscreen] = useState(false);
  const toggleFullscreen = useCallback(async () => {
    const next = !isFullscreen;
    await setPlayerFullscreen(next);
    setIsFullscreen(next);
  }, [isFullscreen]);
  const closePlayer = useCallback(() => {
    setPlayerOpen(false);
    if (isFullscreen) void run(toggleFullscreen);
  }, [isFullscreen, toggleFullscreen]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!preferences.keyboardShortcuts || !player.currentUrl || event.ctrlKey || event.metaKey || event.altKey) return;
      const target = event.target as HTMLElement | null;
      // Text fields keep their keys. Sliders are excluded from that rule so the arrows seek instead of nudging the slider.
      if (target?.closest("input:not([type=range]), textarea, select, [contenteditable=true]")) return;
      // Space on a focused button already activates that button; handling it here would toggle twice.
      if (event.key === " " && target?.closest("button, a")) return;
      if (!playerOpen && document.querySelector("dialog[open]")) return;
      const shortcut = playerShortcutFor(event.key);
      if (!shortcut || (shortcut === "close" && !playerOpen)) return;
      event.preventDefault();
      const actions: Record<PlayerShortcut, () => Promise<unknown>> = {
        "toggle-pause": player.togglePause,
        "seek-back": () => player.seekRelative(-PLAYER_SEEK_STEP_SECONDS),
        "seek-forward": () => player.seekRelative(PLAYER_SEEK_STEP_SECONDS),
        "volume-down": () => player.changeVolume(player.volume - PLAYER_VOLUME_STEP),
        "volume-up": () => player.changeVolume(player.volume + PLAYER_VOLUME_STEP),
        "toggle-mute": player.toggleMute,
        "toggle-fullscreen": toggleFullscreen,
        close: async () => closePlayer(),
      };
      void run(actions[shortcut]);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [preferences.keyboardShortcuts, player, playerOpen, toggleFullscreen, closePlayer]);

  // Files on this computer belong to the Local Device page only; every other tab shows sources.
  const available = useMemo(() => library.channels.filter((channel) => !isLocalPath(channel.url) && (!preferences.hideUnavailable || !channel.last_error)), [library.channels, preferences.hideUnavailable]);
  const catalog = useMemo(() => catalogShelves(available), [available]);
  // The following episode is looked up across the whole library, not just the current search results.
  const followingEpisode = useMemo(() => {
    const current = library.allChannels.find((channel) => channel.id === player.currentChannelId);
    return current ? nextEpisode(current, library.allChannels) : null;
  }, [library.allChannels, player.currentChannelId]);
  const byKind = useMemo(() => ({
    movie: available.filter((channel) => contentKind(channel) === "movie"),
    series: available.filter((channel) => contentKind(channel) === "series"),
    live: available.filter((channel) => contentKind(channel) === "live"),
  }), [available]);
  const myList = useMemo(() => library.allChannels.filter((channel) => library.favoriteIds.has(channel.id) && !isLocalPath(channel.url)), [library.allChannels, library.favoriteIds]);
  // Entries added since this profile last used Lumen. The backend answers the same way for the
  // whole session, so this is "since the previous session" however often it is asked.
  const [newChannelIds, setNewChannelIds] = useState<number[]>([]);
  useEffect(() => {
    if (activeProfileId === null) return;
    let active = true;
    boardNewChannelIds().then((ids) => { if (active) setNewChannelIds(ids); }).catch(() => { if (active) setNewChannelIds([]); });
    return () => { active = false; };
  }, [activeProfileId, library.allChannels]);
  const recommendations = useMemo(() => recommendChannels(available, preferences.personalizedRows ? history : [], preferences.personalizedRows ? library.favoriteIds : new Set()), [available, history, library.favoriteIds, preferences.personalizedRows]);

  const board = useMemo(
    () => buildBoard({ channels: available, history, favorites: library.favoriteIds, newChannelIds, recommended: recommendations }),
    [available, history, library.favoriteIds, newChannelIds, recommendations],
  );

  // The page decides the base set (everything, or one content kind); the facets then
  // narrow it. Group, Genre and Year are all built from that same base set.
  const baseChannels = useMemo(() => {
    if (page === "movies") return byKind.movie;
    if (page === "series") return byKind.series;
    if (page === "live") return byKind.live;
    return available;
  }, [page, byKind, available]);
  const facets = useMemo(() => buildFacets(baseChannels, facetSelection), [baseChannels, facetSelection]);
  const browseChannels = useMemo(() => baseChannels.filter((channel) => matchesFacets(channel, facetSelection)), [baseChannels, facetSelection]);
  const filtered = Boolean(library.searchQuery.trim()) || facetCount(facetSelection) > 0 || library.selectedPlaylistId !== null;

  // Switching sections resets the browsing filters but keeps the top-bar search: someone who
  // typed a query and then opened a section is still looking for that same query.
  const navigate = (next: Page) => { setPage(next); library.setSelectedGroup("all"); library.selectPlaylist(null); setFacetSelection(EMPTY_FACETS); };
  const openSettings = (section: SettingsSection) => { setPage("settings"); setSettingsSection(section); };

  // Map a nav page id to its content kind. Page ids are plural ("movies") while the kind keys
  // are singular, so this translation is required to avoid an undefined lookup.
  const listForPage = (target: Page): Channel[] =>
    target === "movies" ? byKind.movie : target === "series" ? byKind.series : target === "live" ? byKind.live : [];
  const emptyState = (kind: "movies" | "series" | "live") => {
    const title = library.error ? "Your library will be here."
      : kind === "movies" ? "No movies yet."
      : kind === "series" ? "No series yet."
      : "No live channels right now.";
    const body = kind === "live" ? "Live channels come from news, sport, and channel playlists in your sources."
      : "Movies and series come from your own playlists. Import one and they show up here automatically.";
    return <div className="empty-state">
      <div className="empty-symbol"><Icon name="grid" size={32} /></div>
      <h2>{title}</h2>
      <p>{body}</p>
      <button className="primary-button" onClick={() => openSettings("sources")}>Open Settings</button>
    </div>;
  };

  const openSearch = useCallback(() => {
    setSearchOpen(true);
    requestAnimationFrame(() => searchRef.current?.focus());
  }, []);

  // Ctrl/Cmd + K reaches search from anywhere, including while a dialog is closed.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLocaleLowerCase() === "k") {
        event.preventDefault();
        openSearch();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [openSearch]);

  const searchModifier = typeof navigator !== "undefined" && /mac|iphone|ipad/i.test(navigator.userAgent) ? "⌘" : "Ctrl ";

  return <>
    <div className="app" inert={locked}>
    <a className="skip-link" href="#main-content">Skip to content</a>
    <header className="app-header">
      <div className="brand-lockup"><button className="wordmark" onClick={() => navigate("discovery")} aria-label="Lumen home">LUMEN<span>•</span></button><span className="brand-caption">YOUR PERSONAL CINEMA</span></div>
      <nav className="main-nav" aria-label="Main navigation">{pages.map((item) => <button key={item.id} className={page === item.id ? "active" : ""} aria-current={page === item.id ? "page" : undefined} onClick={() => navigate(item.id)}><Icon name={item.icon} size={17} /><span>{item.label}</span></button>)}</nav>
      <div className="header-tools">
        <form className={`header-search ${searchOpen || library.searchQuery ? "open" : ""}`} role="search" onSubmit={(event) => event.preventDefault()}>
          <button type="button" className="search-trigger" aria-label="Search your library" aria-expanded={searchOpen || Boolean(library.searchQuery)} onClick={openSearch}>
            <Icon name="search" size={17} /><span>Search</span><kbd className="search-kbd">{searchModifier}K</kbd>
          </button>
          <label className="search-field">
            <Icon name="search" size={17} />
            <input
              ref={searchRef}
              value={library.searchQuery}
              aria-label="Search your library"
              placeholder="Search titles, categories, files"
              onChange={(event) => {
                const value = event.target.value;
                library.setSearchQuery(value);
                // Search routes to the provider engine while keeping the query in the header.
                if (value && page !== "search") navigate("search");
              }}
              onKeyDown={(event) => { if (event.key === "Escape") { event.currentTarget.blur(); setSearchOpen(false); } }}
              onBlur={(event) => { if (!event.currentTarget.value) setSearchOpen(false); }}
            />
            {library.searchQuery
              ? <button type="button" className="icon-button search-clear" aria-label="Clear search" onMouseDown={(event) => event.preventDefault()} onClick={() => { library.setSearchQuery(""); searchRef.current?.focus(); }}><Icon name="close" size={15} /></button>
              : <kbd className="search-kbd">Esc</kbd>}
          </label>
        </form>
        <button className="icon-button header-save" aria-label="Open My List" onClick={() => openSettings("mylist")}><Icon name="bookmark" size={19} /><span className="header-count">{myList.length}</span></button><span className="header-divider" /><button className="header-profile" aria-label={currentProfile ? `Open settings. Profile: ${currentProfile.name}` : "Open settings"} onClick={() => openSettings(settingsSection)}>{currentProfile ? currentProfile.name.slice(0, 1).toUpperCase() : "L"}</button></div>
    </header>
    <main id="main-content">
      {notice && <div className="app-notice" role="alert"><span>{notice}</span><button className="icon-button" aria-label="Dismiss notice" onClick={() => setNotice(null)}><Icon name="close" /></button></div>}
      {library.error && <div className="app-notice" role="alert"><div><strong>Couldn’t connect to your library</strong><p>{library.error}. Start the Lumen backend or desktop app, then retry.</p></div><button onClick={library.loadChannels}>Retry</button></div>}

      {page === "settings" ? <SettingsPage
        preferences={preferences}
        onPreferencesChange={setPreferences}
        playlists={library.playlists}
        myList={myList}
        onAddSource={() => setShowSource(true)}
        onDeleteSource={library.handleDeletePlaylist}
        onRefresh={() => { setRefresh((value) => value + 1); library.loadChannels(); }}
        onPlayChannel={(channel) => void play(channel)}
        onDetails={setDetails}
        onToggleFavorite={(id) => void library.handleToggleFavorite(id)}
        onBrowseDiscovery={() => navigate("discovery")}
        loadError={library.error}
        section={settingsSection}
        onSectionChange={setSettingsSection}
        profiles={profiles}
        activeProfileId={activeProfileId}
        onProfilesChanged={handleProfilesChanged}
      /> : page === "search" ? <SearchEnginePage
        query={library.searchQuery}
        favorites={library.favoriteIds}
        compact={preferences.compactCards}
        onPlay={(channel) => void play(channel)}
        onPlayExternal={(url, title, headers) => void playFile(url, title, headers)}
        onDetails={setDetails}
        onFavorite={(id) => void library.handleToggleFavorite(id)}
        onRefresh={() => { setRefresh((value) => value + 1); library.loadChannels(); }}
      /> : page === "discovery" ? <DiscoveryPage
        query={library.searchQuery}
        catalog={catalog}
        recommended={recommendations}
        board={board}
        browse={browseTitles}
        personalized={preferences.personalizedRows}
        libraryMatches={available}
        favorites={library.favoriteIds}
        compact={preferences.compactCards}
        onPlayChannel={(channel) => void play(channel)}
        onDetails={setDetails}
        onFavorite={(id) => void library.handleToggleFavorite(id)}
        onPlayFile={(path, name) => void playFile(path, name).catch(() => {})}
        onOpenSources={() => openSettings("sources")}
      /> : page === "local" ? <LocalDevicePage
        query={library.searchQuery}
        history={history}
        historyError={historyError}
        busy={busy}
        onPlayFile={playFile}
        onLibraryChanged={() => { setRefresh((value) => value + 1); library.loadChannels(); }}
        onOpenSources={() => openSettings("sources")}
      /> : <div className="browse-content standalone-content">
          <div className="page-heading"><div><p className="eyebrow">FROM YOUR OWN SOURCES</p><h1>{headings[page as Exclude<Page, "discovery" | "search" | "local" | "settings">].title}</h1><p>{headings[page as Exclude<Page, "discovery" | "search" | "local" | "settings">].blurb}</p></div><span className="library-stat"><strong>{listForPage(page).length}</strong> titles</span></div>

          <div className="browse-toolbar">
            <p className="browse-summary" role="status">
              <strong>{browseChannels.length}</strong> {browseChannels.length === 1 ? "title" : "titles"}
              {facetCount(facetSelection) > 0 && <> · {facetCount(facetSelection)} {facetCount(facetSelection) === 1 ? "category" : "categories"} applied</>}
            </p>
            {filtered && <button className="secondary-button browse-clear" onClick={() => { setFacetSelection(EMPTY_FACETS); library.setSearchQuery(""); library.selectPlaylist(null); }}>Clear filters</button>}
            <label className="source-filter"><span className="sr-only">Filter by source</span><select aria-label="Filter by source" value={library.selectedPlaylistId ?? "all"} onChange={(event) => library.selectPlaylist(event.target.value === "all" ? null : Number(event.target.value))}><option value="all">All sources</option>{library.playlists.map((source) => <option key={source.id} value={source.id}>{source.name}</option>)}</select></label>
          </div>

          {library.loading ? <div className="skeleton-row" role="status" aria-label="Loading library">{[1, 2, 3, 4].map((id) => <div key={id} />)}</div>
            : (page === "movies" || page === "series" || page === "live") && listForPage(page).length === 0 && !filtered ? emptyState(page as "movies" | "series" | "live")
            : baseChannels.length === 0 ? <div className="empty-state"><div className="empty-symbol"><Icon name="grid" size={32} /></div><h2>{library.error ? "Your library will be here." : library.searchQuery ? "No matching titles" : library.allChannels.length ? "Nothing here just yet." : "Your next favorite starts here."}</h2><p>{filtered ? "Try another category, source, or search." : "Add a playlist, explore a free-to-air catalog, or bring your own media."}</p><button className="primary-button" onClick={() => { if (filtered) { setFacetSelection(EMPTY_FACETS); library.setSearchQuery(""); library.selectPlaylist(null); } else openSettings("sources"); }}>{filtered ? "Clear filters" : "Add your first source"}</button></div>
            : <>
              <FacetBrowser
                channels={browseChannels}
                facets={facets}
                selection={facetSelection}
                onSelectionChange={setFacetSelection}
                favorites={library.favoriteIds}
                onPlay={(channel) => void play(channel)}
                onDetails={setDetails}
                onFavorite={(id) => void library.handleToggleFavorite(id)}
                compact={preferences.compactCards}
                emptyMessage={page === "live" ? "No live channels match these categories." : page === "movies" ? "No movies match these categories." : "No series match these categories."}
              />
            </>}
      </div>}
      {page !== "settings" && <div className="browse-content">
        {historyError && <p className="muted-note" role="status">Recently watched is unavailable: {historyError}</p>}
        {profileError && <p className="muted-note" role="status">Profiles are unavailable here: {profileError}</p>}
        <footer><span className="footer-brand">LUMEN</span><span>Your media. Your rules.</span><button onClick={() => openSettings("sources")}>Settings & preferences</button></footer>
      </div>}
    </main>

    {player.currentUrl && <aside className="now-playing-dock" aria-label="Now playing"><button className="dock-title" onClick={() => setPlayerOpen(true)}><span className="dock-play-icon"><Icon name="play" /></span><span><small>NOW PLAYING</small><strong>{player.currentTitle || player.playerState.title}</strong></span></button><button className="secondary-button" onClick={() => setPlayerOpen(true)}>Player controls</button><button className="icon-button" aria-label="Stop playback" onClick={() => void run(player.stopPlayback)}><Icon name="stop" /></button></aside>}
    <SourceDialog isOpen={showSource} onClose={() => setShowSource(false)} onSourceAdded={() => setRefresh((value) => value + 1)} onPlayFile={playFile} />
    <Modal open={details !== null} onClose={() => setDetails(null)} title="Title details" className="details-dialog">{details && <><ChannelArtwork key={details.id} channel={details} /><div className="dialog-content"><p className="eyebrow">{details.group_title || "YOUR LIBRARY"}</p><h2>{details.name}</h2><p>From {library.playlists.find((source) => source.id === details.playlist_id)?.name || "your sources"}. Playback uses native mpv on desktop.</p><p className="muted-note">{details.last_error ? `Last check failed: ${details.last_error}` : details.last_ok ? `Last successful check: ${new Date(details.last_ok * 1000).toLocaleString()}. Availability can change.` : "Availability has not been checked yet."}</p><div className="hero-buttons"><button className="primary-button" disabled={busy} onClick={() => void play(details)}><Icon name="play" />Play</button><button className="secondary-button" aria-pressed={library.favoriteIds.has(details.id)} onClick={() => void library.handleToggleFavorite(details.id)}><Icon name={library.favoriteIds.has(details.id) ? "check" : "plus"} />My List</button><button className="secondary-button" disabled={library.probing} onClick={() => void library.handleProbeVisible([details.id])}>{library.probing ? "Checking…" : "Check stream"}</button></div></div></>}</Modal>
    {player.currentUrl && <PlayerOverlay
      title={player.currentTitle || player.playerState.title || "Now playing"}
      paused={player.playerState.paused}
      hidden={!playerOpen}
      onBack={closePlayer}
      nextEpisodeTitle={followingEpisode?.name}
      onNextEpisode={followingEpisode ? () => void play(followingEpisode) : undefined}
      buffering={player.playerState.buffering}
      onTogglePause={() => void run(player.togglePause)}
      onToggleFullscreen={() => void run(toggleFullscreen)}
      controls={<Transport state={player.playerState} volume={player.volume} muted={player.muted} onPlayPause={() => void run(player.togglePause)} onStop={() => void run(async () => { await player.stopPlayback(); closePlayer(); })} onSeek={(time) => void run(() => player.seekTo(time))} onSeekRelative={(delta) => void run(() => player.seekRelative(delta))} onVolumeChange={(volume) => void run(() => player.changeVolume(volume))} onToggleMute={() => void run(player.toggleMute)} onSelectTrack={(kind, id) => void run(() => player.selectTrack(kind, id))} onSelectQuality={(id) => void run(() => player.selectQuality(id))} isFullscreen={isFullscreen} onToggleFullscreen={() => void run(toggleFullscreen)} />}
    >
      <div className="player-native-card"><Icon name="play" size={40} /><h2>{player.currentTitle}</h2><p>Your video plays in the native mpv window.</p></div>
    </PlayerOverlay>}
    </div>
    {locked && <ProfileGate profiles={profiles} initialProfileId={activeProfileId} onUnlocked={handleProfilesChanged} />}
  </>;
}
export default App;

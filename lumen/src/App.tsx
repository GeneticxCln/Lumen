import { Fragment, useState, useEffect, useMemo, useCallback, useRef } from "react";
import { Transport } from "./components/Transport";
import { PlayerOverlay } from "./components/PlayerOverlay";
import { PLAYER_SEEK_STEP_SECONDS, PLAYER_VOLUME_STEP, playerShortcutFor } from "./lib/playerKeys";
import { nextUp } from "./lib/autoplay";
import { useEpisodeAlerts } from "./hooks/useEpisodeAlerts";
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
import { activeProfile, boardNewChannelIds, setEmbeddedVideo, classifyOpenTargets, importPlaylistFile, takeLaunchTargets, browseTmdb, listEpisodeMarks, setEpisodesWatched, upcomingEpisodes, errorMessage, getHistory, getPlayerPrefs, listProfiles, setPlayerFullscreen, tmdbStatus, updateRestart, updateState } from "./lib/tauri";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { BrowseTitle } from "./lib/types";
import type { CastState, Channel, HistoryEntry, PlayerPrefs, Profile, UpdateEventPayload } from "./lib/types";
import { readPreferences, recommendChannels, catalogShelves, contentKind, isAdult, applyTheme, buildFacets, matchesFacets, facetCount, EMPTY_FACETS } from "./lib/discovery";
import { isLocalPath } from "./lib/local";
import { refreshNetworkPrivacyEnabled } from "./lib/networkPrivacy";
import { nextEpisode } from "./lib/episodes";
import { buildBoard } from "./lib/board";
import { launchNotice, planLaunch } from "./lib/launch";
import type { OpenTarget } from "./lib/launch";
import { describeUpcoming } from "./lib/series";
import type { EpisodeMark, UpcomingSeries } from "./lib/series";
import { EpisodeList } from "./components/EpisodeList";
import { CategoryOverridePicker } from "./components/CategoryOverridePicker";
import { CastBar } from "./components/CastBar";
import { CastDialog } from "./components/CastDialog";
import { FindSubtitles } from "./components/FindSubtitles";
import type { UpcomingCard } from "./components/MediaShelf";
import type { FacetSelection, Preferences } from "./lib/discovery";
import { splitRich, t, tn } from "./i18n/index.ts";
import type { PlainKey, RichPart } from "./i18n/index.ts";
import "./App.css";

type Page = "discovery" | "search" | "movies" | "series" | "live" | "local" | "settings";

// The top bar is exactly these five destinations: Discovery, Movies, Series and Live TV are your
// own sources, and Local Device is the media on this computer. Settings is deliberately absent — the header controls (My
// List and the profile button) and the footer already open it, so an entry would only duplicate
// them. Search is not a destination either: it lives in the top bar on every page.
type NavPage = Exclude<Page, "settings">;

const pages: { id: NavPage; label: PlainKey; icon: IconName }[] = [
  { id: "discovery", label: "app.nav.discovery", icon: "compass" },
  { id: "movies", label: "app.nav.movies", icon: "film" },
  { id: "series", label: "app.nav.series", icon: "series" },
  { id: "live", label: "app.nav.live", icon: "tv" },
  { id: "local", label: "app.nav.local", icon: "monitor" },
];

const headings: Record<Exclude<Page, "discovery" | "search" | "local" | "settings">, { title: PlainKey; blurb: PlainKey }> = {
  movies: { title: "app.heading.movies.title", blurb: "app.heading.movies.blurb" },
  series: { title: "app.heading.series.title", blurb: "app.heading.series.blurb" },
  live: { title: "app.heading.live.title", blurb: "app.heading.live.blurb" },
};

/** Renders a message that marks text with `<strong>`. */
function renderStrong(parts: RichPart[]) {
  return parts.map((part, index) => <Fragment key={index}>{part.tag === "strong" ? <strong>{part.text}</strong> : part.text}</Fragment>);
}

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
  // The profile id this session has confirmed is unlocked (no PIN, or its PIN was just verified).
  // `null` until the first load decides.
  const unlockedProfileId = useRef<number | null>(null);
  const [detailId, setDetailId] = useState<number | null>(null);
  const [facetSelection, setFacetSelection] = useState<FacetSelection>(EMPTY_FACETS);
  const [playerOpen, setPlayerOpen] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  const searchRef = useRef<HTMLInputElement | null>(null);
  const library = useChannels(refresh, { onNotice: (message) => setNotice(message) });
  const [browseTitles, setBrowseTitles] = useState<BrowseTitle[]>([]);
  const player = usePlayer({ onNotice: (message) => setNotice(message) });
  const details = library.allChannels.find((channel) => channel.id === detailId) || null;
  const currentProfile = profiles.find((profile) => profile.id === activeProfileId) || null;
  const setDetails = (channel: Channel | null) => setDetailId(channel?.id ?? null);

  // `verifiedProfileId` is set only when the caller just checked a PIN (or confirmed none is
  // needed) for that exact profile, so a profile switch that did not go through that check (for
  // example the active profile's own deletion silently falling back to another one) is never
  // mistaken for an already-unlocked screen.
  const loadProfiles = useCallback(async (verifiedProfileId?: number) => {
    try {
      const [list, active] = await Promise.all([listProfiles(), activeProfile()]);
      setProfiles(list);
      setActiveProfileId(active.id);
      setProfileError(null);
      if (verifiedProfileId !== undefined) unlockedProfileId.current = verifiedProfileId;
      const needsUnlock = active.has_pin && list.length > 0 && active.id !== unlockedProfileId.current;
      if (!needsUnlock) unlockedProfileId.current = active.id;
      setLocked(needsUnlock);
    } catch (error) {
      // The browser development bridge has no profile API. Say so and fail open rather than
      // hiding the library behind a lock that could never be satisfied.
      setProfiles([]);
      setActiveProfileId(null);
      setLocked(false);
      setProfileError(errorMessage(error));
    }
  }, []);

  useEffect(() => { void loadProfiles(); }, [loadProfiles]);

  // A profile switch (or an unlock) changes whose history and My List are on screen.
  const handleProfilesChanged = async (verifiedProfileId?: number) => {
    await loadProfiles(verifiedProfileId);
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

  // Just for the footer signature; failing silently leaves it blank rather than breaking the page.
  const [appVersion, setAppVersion] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    updateState().then((state) => { if (active) setAppVersion(state.current); }).catch(() => {});
    return () => { active = false; };
  }, []);

  // Whether remote images (TMDB art, channel logos) go through the network-privacy proxy or
  // load directly depends on this; read once so it's right from the first thing rendered.
  useEffect(() => { void refreshNetworkPrivacyEnabled(); }, []);

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

  // Lets the user pick a subtitle file for what is playing now.
  async function chooseSubtitleFile() {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const selected = await open({ filters: [{ name: t("transport.subtitles"), extensions: ["srt", "ass", "ssa", "vtt", "sub"] }] });
    if (typeof selected === "string") await player.addSubtitle(selected);
  }

  // Opens the online subtitle search for what is playing, starting from the profile's language.
  const [findingSubtitles, setFindingSubtitles] = useState(false);
  const [castOpen, setCastOpen] = useState(false);
  const [cast, setCast] = useState<CastState | null>(null);
  const [subtitleLanguage, setSubtitleLanguage] = useState("");
  async function openSubtitleFinder() {
    try {
      setSubtitleLanguage((await getPlayerPrefs()).subtitles);
    } catch {
      setSubtitleLanguage("");
    }
    setFindingSubtitles(true);
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

  // Things opened from outside: "Open with Lumen", a second launch, a lumen:// link, dropped files.
  async function openTargets(targets: OpenTarget[]) {
    const plan = planLaunch(targets);
    let imported = 0;
    for (const path of plan.playlists) {
      try { await importPlaylistFile(path); imported += 1; } catch (error) { setNotice(errorMessage(error)); }
    }
    if (imported > 0) { setRefresh((value) => value + 1); library.loadChannels(); }
    let playing = false;
    if (plan.play?.kind === "media") {
      try { await playFile(plan.play.path); playing = true; } catch { /* playFile already told the user */ }
    } else if (plan.play?.kind === "stream") {
      // Anyone can send a link (a web page can open lumen:// URLs), so ask before playing it.
      const { ask } = await import("@tauri-apps/plugin-dialog");
      if (await ask(t("app.openLink.confirm", { url: plan.play.url }), { title: t("app.openLink.title"), kind: "warning" })) {
        try { await playFile(plan.play.url); playing = true; } catch { /* playFile already told the user */ }
      }
    }
    let withoutVideo = false;
    for (const path of plan.subtitles) {
      if (playing || playerOpen) {
        try { await player.addSubtitle(path); } catch (error) { setNotice(errorMessage(error)); }
      } else withoutVideo = true;
    }
    const message = launchNotice(plan, imported, withoutVideo);
    if (message) setNotice(message);
  }
  const openTargetsRef = useRef(openTargets);
  openTargetsRef.current = openTargets;

  // Listen for items opened while running, then collect what arrived before the page was ready.
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    import("@tauri-apps/api/event").then(async ({ listen }) => {
      const off = await listen<OpenTarget[]>("launch://open", (event) => { void openTargetsRef.current(event.payload); });
      if (disposed) { off(); return; }
      unlisten = off;
      const waiting = await takeLaunchTargets();
      if (!disposed && waiting.length > 0) void openTargetsRef.current(waiting);
    }).catch(() => {});
    return () => { disposed = true; unlisten?.(); };
  }, []);

  // Stop pressed in the desktop's media controls: do what the Stop button does, so progress is saved.
  const stopFromDesktopRef = useRef<() => void>(() => {});
  stopFromDesktopRef.current = () => { void run(async () => { await player.stopPlayback(); closePlayer(); }); };
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    import("@tauri-apps/api/event").then(async ({ listen }) => {
      const off = await listen("mpris://stop", () => stopFromDesktopRef.current());
      if (disposed) off(); else unlisten = off;
    }).catch(() => {});
    return () => { disposed = true; unlisten?.(); };
  }, []);

  // An update installed in the background waits for a restart; a newer release that is not
  // installed automatically is mentioned once.
  const [updateReady, setUpdateReady] = useState<string | null>(null);
  useEffect(() => {
    let disposed = false;
    const offs: Array<() => void> = [];
    import("@tauri-apps/api/event").then(async ({ listen }) => {
      const ready = await listen<UpdateEventPayload>("update://ready", (event) => setUpdateReady(event.payload.tag));
      const available = await listen<UpdateEventPayload>("update://available", (event) => setNotice(t("update.notice.available", { tag: event.payload.tag })));
      if (disposed) { ready(); available(); } else offs.push(ready, available);
    }).catch(() => {});
    return () => { disposed = true; offs.forEach((off) => off()); };
  }, []);

  // Dropping files on the window opens them the same way.
  const [dragging, setDragging] = useState(false);
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    import("@tauri-apps/api/webview").then(async ({ getCurrentWebview }) => {
      const off = await getCurrentWebview().onDragDropEvent((event) => {
        const type = event.payload.type;
        if (type === "enter" || type === "over") setDragging(true);
        else if (type === "leave") setDragging(false);
        else if (type === "drop") {
          setDragging(false);
          classifyOpenTargets(event.payload.paths).then((targets) => {
            if (targets.length > 0) void openTargetsRef.current(targets);
            else setNotice(t("app.drop.nothing"));
          }).catch((error) => setNotice(errorMessage(error)));
        }
      });
      if (disposed) off(); else unlisten = off;
    }).catch(() => {});
    return () => { disposed = true; unlisten?.(); };
  }, []);

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

  // Playing inside the window (experimental): give the video room above the page while the player
  // is open, keep it sized as the window changes, and hand the room back when it closes.
  const [embeddedActive, setEmbeddedActive] = useState(false);
  const videoHeightRef = useRef(0);
  useEffect(() => {
    if (!player.currentUrl || !playerOpen) {
      if (videoHeightRef.current > 0) {
        videoHeightRef.current = 0;
        void setEmbeddedVideo(false, 0).catch(() => {});
      }
      setEmbeddedActive(false);
      return;
    }
    let active = true;
    const apply = async () => {
      // The page only sees the room left under the video, so the window height is both together.
      const total = window.innerHeight + videoHeightRef.current;
      const height = Math.round(total * (isFullscreen ? 0.85 : 0.62));
      try {
        const showing = await setEmbeddedVideo(true, height);
        if (!active) return;
        videoHeightRef.current = showing ? height : 0;
        setEmbeddedActive(showing);
      } catch {
        if (active) setEmbeddedActive(false);
      }
    };
    void apply();
    const onResize = () => void apply();
    window.addEventListener("resize", onResize);
    return () => { active = false; window.removeEventListener("resize", onResize); };
  }, [player.currentUrl, playerOpen, isFullscreen]);


  // The profile's player preferences the page itself uses: how far to skip, and the next-episode countdown.
  const [playerPrefs, setPlayerPrefsState] = useState<PlayerPrefs | null>(null);
  useEffect(() => {
    let active = true;
    getPlayerPrefs().then((loaded) => { if (active) setPlayerPrefsState(loaded); }).catch(() => {});
    return () => { active = false; };
  }, [player.currentUrl, playerOpen, activeProfileId]);
  const seekStep = playerPrefs?.seekStep ?? PLAYER_SEEK_STEP_SECONDS;

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!preferences.keyboardShortcuts || !player.currentUrl || event.ctrlKey || event.metaKey || event.altKey) return;
      const target = event.target as HTMLElement | null;
      // Text fields keep their keys. Sliders are excluded from that rule so the arrows seek instead of nudging the slider.
      if (target?.closest("input:not([type=range]), textarea, select, [contenteditable=true]")) return;
      // Space on a focused button already activates that button; handling it here would toggle twice.
      if (event.key === " " && target?.closest("button, a")) return;
      if (!playerOpen && document.querySelector("dialog[open]")) return;
      // The subtitle search is a dialog over the player: its keys (Escape closes only the dialog) belong to it.
      if (findingSubtitles || castOpen) return;
      const shortcut = playerShortcutFor(event.key);
      if (!shortcut || (shortcut === "close" && !playerOpen)) return;
      event.preventDefault();
      const actions: Record<PlayerShortcut, () => Promise<unknown>> = {
        "toggle-pause": player.togglePause,
        "seek-back": () => player.seekRelative(-seekStep),
        "seek-forward": () => player.seekRelative(seekStep),
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
  }, [preferences.keyboardShortcuts, player, playerOpen, findingSubtitles, castOpen, seekStep, toggleFullscreen, closePlayer]);

  // Files on this computer belong to the Local Device page only; every other tab shows sources.
  const available = useMemo(() => library.channels.filter((channel) =>
    !isLocalPath(channel.url) &&
    (!preferences.hideUnavailable || !channel.last_error) &&
    (preferences.showAdult || !isAdult(channel))
  ), [library.channels, preferences.hideUnavailable, preferences.showAdult]);
  const catalog = useMemo(() => catalogShelves(available), [available]);
  // What's playing, looked up across the whole library so the idle/buffering player stage can
  // show its art as a backdrop instead of flat black. Undefined for an external (non-library) URL.
  const currentChannel = useMemo(
    () => library.allChannels.find((channel) => channel.id === player.currentChannelId),
    [library.allChannels, player.currentChannelId],
  );
  // The following episode is looked up across the whole library, not just the current search results.
  const followingEpisode = useMemo(() => {
    const current = library.allChannels.find((channel) => channel.id === player.currentChannelId);
    return current ? nextEpisode(current, library.allChannels) : null;
  }, [library.allChannels, player.currentChannelId]);
  // Desktop notifications for new episodes of followed series, when the user turned them on.
  useEpisodeAlerts(activeProfileId);

  // Offer the next episode in the last seconds, and start it when this one ends.
  const [dismissedNextFor, setDismissedNextFor] = useState<string | null>(null);
  const upNext = nextUp({
    time: player.playerState.time,
    duration: player.playerState.duration,
    paused: player.playerState.paused,
    countdown: playerPrefs?.autoplayCountdown ?? 10,
    hasNext: followingEpisode !== null,
    dismissed: dismissedNextFor !== null && dismissedNextFor === player.currentUrl,
  });
  const startNextNow = upNext?.startNow === true;
  const playRef = useRef(play);
  playRef.current = play;
  const autoplayedFor = useRef<string | null>(null);
  useEffect(() => {
    // Cleared once the next episode is playing, so replaying this one can chain again.
    if (!startNextNow) { autoplayedFor.current = null; return; }
    if (!followingEpisode || !player.currentUrl || autoplayedFor.current === player.currentUrl) return;
    autoplayedFor.current = player.currentUrl;
    void playRef.current(followingEpisode);
  }, [startNextNow, followingEpisode, player.currentUrl]);
  const byKind = useMemo(() => ({
    movie: available.filter((channel) => contentKind(channel) === "movie"),
    series: available.filter((channel) => contentKind(channel) === "series"),
    live: available.filter((channel) => contentKind(channel) === "live"),
  }), [available]);
  // Favorites always show regardless of availability (unlike `available`, there's no
  // `hideUnavailable` check here), but the Adult toggle is a content-visibility policy, not an
  // availability signal, so it still applies here the same as everywhere else.
  const myList = useMemo(() => library.allChannels.filter((channel) => library.favoriteIds.has(channel.id) && !isLocalPath(channel.url) && (preferences.showAdult || !isAdult(channel))), [library.allChannels, library.favoriteIds, preferences.showAdult]);
  // Entries added since this profile last used Lumen. The backend answers the same way for the
  // whole session, so this is "since the previous session" however often it is asked.
  const [newChannelIds, setNewChannelIds] = useState<number[]>([]);
  // Watched marks set by hand, and the schedules of followed series.
  const [marks, setMarks] = useState<EpisodeMark[]>([]);
  const [upcoming, setUpcoming] = useState<UpcomingSeries[]>([]);
  useEffect(() => {
    if (activeProfileId === null) return;
    let active = true;
    listEpisodeMarks().then((list) => { if (active) setMarks(list ?? []); }).catch(() => { if (active) setMarks([]); });
    return () => { active = false; };
  }, [activeProfileId, refresh]);
  useEffect(() => {
    if (activeProfileId === null) return;
    let active = true;
    // Show what is stored at once, then let TMDB refresh stale schedules in the background.
    upcomingEpisodes(false).then((list) => { if (active) setUpcoming(list ?? []); }).catch(() => {});
    upcomingEpisodes(true).then((list) => { if (active) setUpcoming(list ?? []); }).catch(() => {});
    return () => { active = false; };
  }, [activeProfileId]);
  const markEpisodes = async (seriesKey: string, season: number, episodes: number[], watched: boolean) => {
    await run(async () => {
      await setEpisodesWatched(seriesKey, season, episodes, watched);
      setMarks((await listEpisodeMarks()) ?? []);
    });
  };
  const today = new Date().toISOString().slice(0, 10);
  const upcomingCards = useMemo<UpcomingCard[]>(() => upcoming.flatMap((item) => {
    const label = describeUpcoming(item, today);
    if (!label) return [];
    return [{ key: `tv-${item.tmdbId}`, name: item.name, text: label.text, kind: label.kind, channel: library.allChannels.find((channel) => channel.id === item.channelId) }];
  }), [upcoming, today, library.allChannels]);
  useEffect(() => {
    if (activeProfileId === null) return;
    let active = true;
    boardNewChannelIds().then((ids) => { if (active) setNewChannelIds(ids ?? []); }).catch(() => { if (active) setNewChannelIds([]); });
    return () => { active = false; };
  }, [activeProfileId, library.allChannels]);
  const recommendations = useMemo(() => recommendChannels(available, preferences.personalizedRows ? history : [], preferences.personalizedRows ? library.favoriteIds : new Set()), [available, history, library.favoriteIds, preferences.personalizedRows]);

  const board = useMemo(
    () => buildBoard({ channels: available, history, favorites: library.favoriteIds, newChannelIds, recommended: recommendations, marks }),
    [available, history, library.favoriteIds, newChannelIds, recommendations, marks],
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
    const title = library.error ? t("app.empty.libraryWillBeHere")
      : kind === "movies" ? t("app.empty.noMovies")
      : kind === "series" ? t("app.empty.noSeries")
      : t("app.empty.noLive");
    const body = kind === "live" ? t("app.empty.liveBody")
      : t("app.empty.moviesSeriesBody");
    return <div className="empty-state">
      <div className="empty-symbol"><Icon name="grid" size={32} /></div>
      <h2>{title}</h2>
      <p>{body}</p>
      <button className="primary-button" onClick={() => openSettings("sources")}>{t("app.empty.openSettings")}</button>
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

  const searchShortcut = typeof navigator !== "undefined" && /mac|iphone|ipad/i.test(navigator.userAgent) ? t("app.search.shortcutMac") : t("app.search.shortcutOther");

  const detailSource = details ? library.playlists.find((source) => source.id === details.playlist_id)?.name : undefined;

  return <>
    <div className={`app${player.currentUrl ? " has-dock" : ""}`} inert={locked}>
    <a className="skip-link" href="#main-content">{t("app.skipToContent")}</a>
    <header className="app-header">
      <div className="brand-lockup"><button className="wordmark" onClick={() => navigate("discovery")} aria-label={t("app.brand.home")}>{t("app.brand.name")}<span>•</span></button><span className="brand-caption">{t("app.brand.caption")}</span></div>
      <nav className="main-nav" aria-label={t("app.nav.label")}>{pages.map((item) => <button key={item.id} className={page === item.id ? "active" : ""} aria-current={page === item.id ? "page" : undefined} onClick={() => navigate(item.id)}><Icon name={item.icon} size={17} /><span>{t(item.label)}</span></button>)}</nav>
      <div className="header-tools">
        <form className={`header-search ${searchOpen || library.searchQuery ? "open" : ""}`} role="search" onSubmit={(event) => event.preventDefault()}>
          <button type="button" className="search-trigger" aria-label={t("app.search.label")} aria-expanded={searchOpen || Boolean(library.searchQuery)} onClick={openSearch}>
            <Icon name="search" size={17} /><span>{t("app.search.button")}</span><kbd className="search-kbd">{searchShortcut}</kbd>
          </button>
          <label className="search-field">
            <Icon name="search" size={17} />
            <input
              ref={searchRef}
              value={library.searchQuery}
              aria-label={t("app.search.label")}
              placeholder={t("app.search.placeholder")}
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
              ? <button type="button" className="icon-button search-clear" aria-label={t("app.search.clear")} onMouseDown={(event) => event.preventDefault()} onClick={() => { library.setSearchQuery(""); searchRef.current?.focus(); }}><Icon name="close" size={15} /></button>
              : <kbd className="search-kbd">{t("app.search.escKey")}</kbd>}
          </label>
        </form>
        <button className="icon-button header-save" aria-label={t("app.header.openMyList")} onClick={() => openSettings("mylist")}><Icon name="bookmark" size={19} /><span className="header-count">{myList.length}</span></button><span className="header-divider" /><button className="header-profile" aria-label={currentProfile ? t("app.header.openSettingsProfile", { name: currentProfile.name }) : t("app.header.openSettings")} onClick={() => openSettings(settingsSection)}>{currentProfile ? currentProfile.name.slice(0, 1).toUpperCase() : t("app.header.profileFallback")}</button></div>
    </header>
    <main id="main-content">
      {dragging && <div className="drop-overlay" aria-hidden="true"><div>{t("app.drop.overlay")}</div></div>}
      {notice && <div className="app-notice" role="alert"><span>{notice}</span><button className="icon-button" aria-label={t("app.notice.dismiss")} onClick={() => setNotice(null)}><Icon name="close" /></button></div>}
      {library.error && <div className="app-notice" role="alert"><div><strong>{t("app.error.connectTitle")}</strong><p>{t("app.error.connectBody", { error: library.error })}</p></div><button onClick={library.loadChannels}>{t("app.error.retry")}</button></div>}

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
        upcoming={upcomingCards}
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
          <div className="page-heading"><div><p className="eyebrow">{t("app.browse.eyebrow")}</p><h1>{t(headings[page as Exclude<Page, "discovery" | "search" | "local" | "settings">].title)}</h1><p>{t(headings[page as Exclude<Page, "discovery" | "search" | "local" | "settings">].blurb)}</p></div><span className="library-stat">{renderStrong(splitRich(tn("app.browse.libraryStat", listForPage(page).length)))}</span></div>

          <div className="browse-toolbar">
            <p className="browse-summary" role="status">
              {renderStrong(splitRich(tn("app.browse.count", browseChannels.length)))}
              {facetCount(facetSelection) > 0 && <> {tn("app.browse.filtersApplied", facetCount(facetSelection))}</>}
            </p>
            {filtered && <button className="secondary-button browse-clear" onClick={() => { setFacetSelection(EMPTY_FACETS); library.setSearchQuery(""); library.selectPlaylist(null); }}>{t("app.browse.clearFilters")}</button>}
            <label className="source-filter"><span className="sr-only">{t("app.browse.filterBySource")}</span><select aria-label={t("app.browse.filterBySource")} value={library.selectedPlaylistId ?? "all"} onChange={(event) => library.selectPlaylist(event.target.value === "all" ? null : Number(event.target.value))}><option value="all">{t("app.browse.allSources")}</option>{library.playlists.map((source) => <option key={source.id} value={source.id}>{source.name}</option>)}</select></label>
          </div>

          {library.loading ? <div className="skeleton-row" role="status" aria-label={t("app.browse.loading")}>{[1, 2, 3, 4].map((id) => <div key={id} />)}</div>
            : (page === "movies" || page === "series" || page === "live") && listForPage(page).length === 0 && !filtered ? emptyState(page as "movies" | "series" | "live")
            : baseChannels.length === 0 ? <div className="empty-state"><div className="empty-symbol"><Icon name="grid" size={32} /></div><h2>{library.error ? t("app.empty.libraryWillBeHere") : library.searchQuery ? t("app.empty.noMatchingTitles") : library.allChannels.length ? t("app.empty.nothingYet") : t("app.empty.firstFavorite")}</h2><p>{filtered ? t("app.empty.tryAnother") : t("app.empty.addOrExplore")}</p><button className="primary-button" onClick={() => { if (filtered) { setFacetSelection(EMPTY_FACETS); library.setSearchQuery(""); library.selectPlaylist(null); } else openSettings("sources"); }}>{filtered ? t("app.browse.clearFilters") : t("app.empty.addFirstSource")}</button></div>
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
                emptyMessage={page === "live" ? t("app.empty.liveMatch") : page === "movies" ? t("app.empty.moviesMatch") : t("app.empty.seriesMatch")}
              />
            </>}
      </div>}
      {page !== "settings" && <div className="browse-content">
        {historyError && <p className="muted-note" role="status">{t("app.history.unavailable", { error: historyError })}</p>}
        {profileError && <p className="muted-note" role="status">{t("app.profiles.unavailable", { error: profileError })}</p>}
        <footer><span className="footer-brand">{t("app.brand.name")}</span><span>{t("app.footer.tagline")}</span>{appVersion && <span>{t("app.footer.version", { version: appVersion })}</span>}<button onClick={() => openSettings("sources")}>{t("app.footer.settings")}</button><button onClick={() => void openUrl("https://github.com/GeneticxCln/Lumen")}>{t("app.footer.github")}</button></footer>
      </div>}
    </main>

    {player.currentUrl && <aside className="now-playing-dock" aria-label={t("app.nowPlaying.label")}><button className="dock-title" onClick={() => setPlayerOpen(true)}><span className="dock-play-icon"><Icon name="play" /></span><span><small>{t("app.nowPlaying.eyebrow")}</small><strong>{player.currentTitle || player.playerState.title}</strong></span></button><button className="secondary-button" onClick={() => setPlayerOpen(true)}>{t("app.nowPlaying.controls")}</button><button className="icon-button" aria-label={t("transport.stop")} onClick={() => void run(player.stopPlayback)}><Icon name="stop" /></button></aside>}
    <SourceDialog isOpen={showSource} onClose={() => setShowSource(false)} onSourceAdded={() => setRefresh((value) => value + 1)} onPlayFile={playFile} />
    <Modal open={details !== null} onClose={() => setDetails(null)} title={t("app.details.title")} className="details-dialog">{details && <><ChannelArtwork key={details.id} channel={details} /><div className="dialog-content"><p className="eyebrow">{details.group_title || t("app.details.eyebrow")}</p><h2>{details.name}</h2><p>{detailSource ? t("app.details.fromSource", { source: detailSource }) : t("app.details.fromYourSources")}</p><p className="muted-note">{details.last_error ? t("app.details.lastFailed", { error: details.last_error }) : details.last_ok ? t("app.details.lastOk", { when: new Date(details.last_ok * 1000).toLocaleString() }) : t("app.details.neverChecked")}</p>{!details.tmdb_type && <CategoryOverridePicker key={details.id} channel={details} onSave={(kind, genres) => library.handleSetCategoryOverride(details.id, kind, genres)} />}<div className="hero-buttons"><button className="primary-button" disabled={busy} onClick={() => void play(details)}><Icon name="play" />{t("transport.play")}</button><button className="secondary-button" aria-pressed={library.favoriteIds.has(details.id)} onClick={() => void library.handleToggleFavorite(details.id)}><Icon name={library.favoriteIds.has(details.id) ? "check" : "plus"} />{t("app.myList")}</button><button className="secondary-button" disabled={library.probing} onClick={() => void library.handleProbeVisible([details.id])}>{library.probing ? t("app.details.checking") : t("app.details.checkStream")}</button></div><EpisodeList current={details} channels={library.allChannels} marks={marks} history={history} busy={busy} onPlay={(channel) => void play(channel)} onMark={(key, season, episodes, watched) => void markEpisodes(key, season, episodes, watched)} /></div></>}</Modal>
    {player.currentUrl && <PlayerOverlay
      title={player.currentTitle || player.playerState.title || t("app.nowPlaying.label")}
      paused={player.playerState.paused}
      hidden={!playerOpen}
      onBack={closePlayer}
      nextEpisodeTitle={followingEpisode?.name}
      onNextEpisode={followingEpisode ? () => void play(followingEpisode) : undefined}
      nextUp={upNext && followingEpisode ? { secondsLeft: upNext.secondsLeft, title: followingEpisode.name } : null}
      onDismissNext={() => setDismissedNextFor(player.currentUrl)}
      buffering={player.playerState.buffering}
      controlsAlwaysVisible={embeddedActive}
      onTogglePause={() => void run(player.togglePause)}
      onToggleFullscreen={() => void run(toggleFullscreen)}
      controls={<Transport state={player.playerState} volume={player.volume} muted={player.muted} onPlayPause={() => void run(player.togglePause)} onStop={() => void run(async () => { await player.stopPlayback(); closePlayer(); })} onSeek={(time) => void run(() => player.seekTo(time))} onSeekRelative={(delta) => void run(() => player.seekRelative(delta))} onVolumeChange={(volume) => void run(() => player.changeVolume(volume))} onToggleMute={() => void run(player.toggleMute)} onSelectTrack={(kind, id) => void run(() => player.selectTrack(kind, id))} onSelectQuality={(id) => void run(() => player.selectQuality(id))} subtitleDelay={player.subtitleDelay} onSubtitleDelay={(direction) => void run(() => player.changeSubtitleDelay(direction))} onSubtitleScale={(direction) => void run(() => player.changeSubtitleScale(direction))} onAddSubtitle={() => void run(chooseSubtitleFile)} onFindSubtitles={() => void run(openSubtitleFinder)} onCast={() => setCastOpen(true)} seekStep={seekStep} speed={player.speed} onSpeed={(speed) => void run(() => player.changeSpeed(speed))} isFullscreen={isFullscreen} onToggleFullscreen={() => void run(toggleFullscreen)} />}
    >
      {embeddedActive
        ? null
        : <div className="player-native-card">
            {currentChannel && <ChannelArtwork key={currentChannel.id} channel={currentChannel} hero />}
            <div className="player-native-card-inner"><Icon name="play" size={40} /><h2>{player.currentTitle}</h2><p>{t("app.nowPlaying.nativeNote")}</p></div>
          </div>}
    </PlayerOverlay>}
    </div>
    <FindSubtitles
      open={findingSubtitles}
      item={player.currentUrl ? { channelId: player.currentChannelId, source: player.currentUrl, name: player.currentTitle || undefined } : null}
      preferredLanguage={subtitleLanguage}
      onClose={() => setFindingSubtitles(false)}
    />
    <CastDialog
      open={castOpen}
      item={player.currentUrl ? { channelId: player.currentChannelId, source: player.currentUrl, name: player.currentTitle || undefined } : null}
      startAt={player.playerState.time}
      onStarted={(state) => {
        setCast(state);
        setCastOpen(false);
        void run(async () => { await player.stopPlayback(); closePlayer(); });
      }}
      onClose={() => setCastOpen(false)}
    />
    {cast && <CastBar cast={cast} seekStep={seekStep} onEnded={() => setCast(null)} onError={setNotice} />}
    {updateReady && (
      <div className="update-banner" role="status">
        <span>{t("update.notice.ready", { tag: updateReady })}</span>
        <button className="primary-button" onClick={() => void run(updateRestart)}>{t("update.restart")}</button>
        <button className="secondary-button" onClick={() => setUpdateReady(null)}>{t("update.later")}</button>
      </div>
    )}
    {locked && <ProfileGate profiles={profiles} initialProfileId={activeProfileId} onUnlocked={handleProfilesChanged} />}
  </>;
}
export default App;

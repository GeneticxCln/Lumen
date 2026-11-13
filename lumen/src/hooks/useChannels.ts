import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import {
  listPlaylists,
  listChannelsPage,
  listChannelGroups,
  listFavoriteChannels,
  listChannelsByIds,
  listFavorites,
  probeUrls,
  toggleFavorite,
  setCategoryOverride,
  deletePlaylist,
  refreshPlaylist,
  errorMessage,
} from "../lib/tauri";
import type { Channel, Playlist } from "../lib/types";
import { filterChannels, type ContentKind } from "../lib/discovery";
import { t } from "../i18n/index.ts";

/** One screenful. The rest is fetched only when the user asks for it. */
const PAGE_SIZE = 500;
const NONE_GROUP = "Other";

interface ChannelsOptions {
  /** Told when some (not all) sources failed to load, so the library still shows what did. */
  onNotice?: (message: string) => void;
}

/** How many hydrated-by-id channels (history, details, search hits) are kept beside the page. */
const EXTRA_CHANNEL_LIMIT = 3000;

function mergeChannels(...groups: Channel[][]): Channel[] {
  const byId = new Map<number, Channel>();
  for (const group of groups) {
    for (const channel of group) byId.set(channel.id, channel);
  }
  return [...byId.values()];
}

export function useChannels(refreshTrigger = 0, options: ChannelsOptions = {}) {
  const onNoticeRef = useRef(options.onNotice);
  onNoticeRef.current = options.onNotice;
  const [playlists, setPlaylists] = useState<Playlist[]>([]);
  const [selectedPlaylistId, setSelectedPlaylistId] = useState<number | null>(null);
  const [pageChannels, setPageChannels] = useState<Channel[]>([]);
  const [favoriteChannels, setFavoriteChannels] = useState<Channel[]>([]);
  // Channels referenced by history or "new since last visit" that are not on the loaded page.
  // Bounded by the history/new-id counts, so this stays small on a 14.76M-row library.
  const [extraChannels, setExtraChannels] = useState<Channel[]>([]);
  const [groups, setGroups] = useState<string[]>([]);
  const [nextAfterName, setNextAfterName] = useState<string | null>(null);
  const [nextAfterId, setNextAfterId] = useState<number | null>(null);
  const [hasMore, setHasMore] = useState(false);
  const [substring, setSubstring] = useState(false);
  const [selectedGroup, setSelectedGroup] = useState("all");
  const [searchQuery, setSearchQuery] = useState("");
  const [favoritesOnly, setFavoritesOnly] = useState(false);
  const [favoriteIds, setFavoriteIds] = useState<Set<number>>(new Set());
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [probing, setProbing] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const reload = useCallback(() => setRevision((value) => value + 1), []);
  // The skeleton belongs to the first load only. A later reload (after "Check stream", an import,
  // a sync) refreshes the data in place; blanking the whole library for it made one small action
  // look like the app had restarted.
  const loadedOnce = useRef(false);
  // Every first-page request invalidates an in-flight "load more", so a filter change can never
  // append a page from the previous filter.
  const requestId = useRef(0);
  const loadingMoreRef = useRef(false);

  // Sources and My List are independent of the current filter page: favorites are fetched by id
  // so they still appear when they are nowhere near the first page of 14.76M channels.
  useEffect(() => {
    let cancelled = false;
    async function loadSources() {
      try {
        const [sources, favorites, favoriteRows] = await Promise.all([
          listPlaylists(),
          listFavorites(),
          listFavoriteChannels(),
        ]);
        if (cancelled) return;
        setPlaylists(sources);
        setFavoriteIds(new Set(favorites));
        setFavoriteChannels(favoriteRows);
        // A deleted source's channels must not linger (or stay playable) in what was hydrated by id.
        const live = new Set(sources.map((source) => source.id));
        setExtraChannels((current) => {
          const kept = current.filter((channel) => live.has(channel.playlist_id));
          return kept.length === current.length ? current : kept;
        });
        setSelectedPlaylistId((current) =>
          current !== null && sources.some((source) => source.id === current) ? current : null,
        );
      } catch (failure) {
        if (!cancelled) setError(errorMessage(failure));
      }
    }
    void loadSources();
    return () => { cancelled = true; };
  }, [refreshTrigger, revision]);

  // Group options come from SQL counts, not from whatever page happens to be loaded.
  useEffect(() => {
    let cancelled = false;
    listChannelGroups(selectedPlaylistId)
      .then((rows) => {
        if (cancelled) return;
        const counts = new Map<string, number>();
        for (const row of rows) {
          const name = row.name || NONE_GROUP;
          counts.set(name, (counts.get(name) ?? 0) + row.count);
        }
        setGroups([...counts.keys()].sort((a, b) => a.localeCompare(b)));
      })
      .catch(() => { if (!cancelled) setGroups([]); });
    return () => { cancelled = true; };
  }, [selectedPlaylistId, refreshTrigger, revision]);

  useEffect(() => {
    let cancelled = false;
    const request = ++requestId.current;
    if (!loadedOnce.current) setLoading(true);
    setError(null);
    // Typing must not fire one query per keystroke against a 14.76M-row table.
    const timer = window.setTimeout(async () => {
      try {
        const page = await listChannelsPage(
          selectedPlaylistId,
          searchQuery,
          selectedGroup === "all" ? null : selectedGroup,
          null,
          null,
          PAGE_SIZE,
        );
        if (cancelled || request !== requestId.current) return;
        setPageChannels(page.channels);
        setNextAfterName(page.next_after_name);
        setNextAfterId(page.next_after_id);
        setHasMore(page.has_more);
        setSubstring(page.substring === true);
        loadedOnce.current = true;
      } catch (failure) {
        if (!cancelled && request === requestId.current) setError(errorMessage(failure));
      } finally {
        if (!cancelled && request === requestId.current) setLoading(false);
      }
    }, searchQuery.trim() ? 250 : 0);
    return () => { cancelled = true; window.clearTimeout(timer); };
  }, [selectedPlaylistId, selectedGroup, searchQuery, refreshTrigger, revision]);

  const loadMore = useCallback(async () => {
    if (nextAfterName === null || nextAfterId === null || !hasMore || loadingMoreRef.current) return;
    loadingMoreRef.current = true;
    const request = requestId.current;
    setLoadingMore(true);
    setError(null);
    try {
      const page = await listChannelsPage(
        selectedPlaylistId,
        searchQuery,
        selectedGroup === "all" ? null : selectedGroup,
        nextAfterName,
        nextAfterId,
        PAGE_SIZE,
        substring,
      );
      if (request !== requestId.current) return;
      setPageChannels((current) => mergeChannels(current, page.channels));
      setNextAfterName(page.next_after_name);
      setNextAfterId(page.next_after_id);
      setHasMore(page.has_more);
    } catch (failure) {
      if (request === requestId.current) setError(errorMessage(failure));
    } finally {
      loadingMoreRef.current = false;
      setLoadingMore(false);
    }
  }, [hasMore, nextAfterId, nextAfterName, searchQuery, selectedGroup, selectedPlaylistId, substring]);

  const ensureChannels = useCallback(async (ids: number[]) => {
    const wanted = [...new Set(ids)].filter((id) => Number.isInteger(id));
    if (wanted.length === 0) return;
    try {
      const rows = await listChannelsByIds(wanted);
      setExtraChannels((current) => mergeChannels(current, rows).slice(-EXTRA_CHANNEL_LIMIT));
    } catch {
      // The board simply omits channels it could not hydrate; playback is unaffected.
    }
  }, []);

  const allChannels = useMemo(
    () => mergeChannels(extraChannels, favoriteChannels, pageChannels),
    [extraChannels, favoriteChannels, pageChannels],
  );
  // This hook powers the Sources management page too, where every loaded channel must stay visible
  // and manageable regardless of the "hide unavailable"/"show adult content" preferences — those
  // only gate what the Home/Discover/My List pages show, not what you can see and edit here.
  //
  // The page was already filtered (search, source, group) by the database, with matching rules
  // the client cannot reproduce (word-prefix full-text search, diacritics, tvg-id), so it is kept
  // as is; only the channels hydrated by id are filtered here.
  const channels = useMemo(() => {
    const onPage = new Set(pageChannels.map((channel) => channel.id));
    const others = [...extraChannels, ...favoriteChannels].filter((channel) =>
      !onPage.has(channel.id) && (selectedPlaylistId === null || channel.playlist_id === selectedPlaylistId));
    return mergeChannels(filterChannels(others, searchQuery, selectedGroup, false, true), pageChannels)
      .filter((channel) => !favoritesOnly || favoriteIds.has(channel.id));
  }, [pageChannels, extraChannels, favoriteChannels, selectedPlaylistId, searchQuery, selectedGroup, favoritesOnly, favoriteIds]);
  const selectPlaylist = useCallback((id: number | null) => {
    setSelectedGroup("all");
    setSelectedPlaylistId(id);
  }, []);

  const handleProbeVisible = useCallback(async (ids?: number[]) => {
    const target = ids || channels.slice(0, 32).map((channel) => channel.id);
    if (!target.length) return;
    setProbing(true);
    setError(null);
    try {
      await probeUrls(target);
      reload();
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setProbing(false);
    }
  }, [channels, reload]);

  const handleToggleFavorite = useCallback(async (id: number) => {
    try {
      await toggleFavorite(id);
      // A favorite belongs to the title: turning it off clears every source of it, and one added
      // from a search hit or a shelf may sit outside the loaded page. Re-reading both lists keeps
      // the badge, the heart and My List in step with what the database now says.
      const [favorites, favoriteRows] = await Promise.all([listFavorites(), listFavoriteChannels()]);
      setFavoriteIds(new Set(favorites));
      setFavoriteChannels(favoriteRows);
    } catch (failure) {
      setError(errorMessage(failure));
    }
  }, []);

  const handleSetCategoryOverride = useCallback(async (id: number, kind: ContentKind | null, genres: string[] | null) => {
    try {
      await setCategoryOverride(id, kind, genres);
      reload();
    } catch (failure) {
      setError(errorMessage(failure));
      throw failure;
    }
  }, [reload]);

  const handleDeletePlaylist = useCallback(async (id: number) => {
    try {
      await deletePlaylist(id);
      reload();
    } catch (failure) {
      setError(errorMessage(failure));
      throw failure;
    }
  }, [reload]);

  const handleRefreshPlaylist = useCallback(async (id: number) => {
    setRefreshing(true);
    setError(null);
    try {
      const stats = await refreshPlaylist(id);
      reload();
      onNoticeRef.current?.(t("channels.refreshDone", stats));
      return stats;
    } catch (failure) {
      setError(errorMessage(failure));
      throw failure;
    } finally {
      setRefreshing(false);
    }
  }, [reload]);

  const addChannels = useCallback((channels: Channel[]) => {
    setExtraChannels((current) => mergeChannels(current, channels).slice(-EXTRA_CHANNEL_LIMIT));
  }, []);

  return { playlists, selectedPlaylistId, selectPlaylist, allChannels, ensureChannels, addChannels, channels, groups, selectedGroup, setSelectedGroup,
    searchQuery, setSearchQuery, favoritesOnly, setFavoritesOnly, favoriteIds, loading, loadingMore, hasMore, loadMore,
    probing, refreshing, error,
    loadPlaylists: reload, loadChannels: reload, handleProbeVisible, handleToggleFavorite, handleSetCategoryOverride,
    handleDeletePlaylist, handleRefreshPlaylist };
}

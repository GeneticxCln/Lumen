import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import { listPlaylists, listChannels, probeUrls, toggleFavorite, setCategoryOverride, listFavorites, deletePlaylist, refreshPlaylist, errorMessage } from "../lib/tauri";
import type { Channel, Playlist } from "../lib/types";
import { filterChannels, type ContentKind } from "../lib/discovery";
import { t, tn } from "../i18n/index.ts";

interface ChannelsOptions {
  /** Told when some (not all) sources failed to load, so the library still shows what did. */
  onNotice?: (message: string) => void;
}

export function useChannels(refreshTrigger = 0, options: ChannelsOptions = {}) {
  const onNoticeRef = useRef(options.onNotice);
  onNoticeRef.current = options.onNotice;
  const [playlists, setPlaylists] = useState<Playlist[]>([]);
  const [selectedPlaylistId, setSelectedPlaylistId] = useState<number | null>(null);
  const [allChannels, setAllChannels] = useState<Channel[]>([]);
  const [selectedGroup, setSelectedGroup] = useState("all");
  const [searchQuery, setSearchQuery] = useState("");
  const [favoritesOnly, setFavoritesOnly] = useState(false);
  const [favoriteIds, setFavoriteIds] = useState<Set<number>>(new Set());
  const [loading, setLoading] = useState(true);
  const [probing, setProbing] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const reload = useCallback(() => setRevision((value) => value + 1), []);
  // The skeleton belongs to the first load only. A later reload (after "Check stream", an import,
  // a sync) refreshes the data in place; blanking the whole library for it made one small action
  // look like the app had restarted.
  const loadedOnce = useRef(false);

  useEffect(() => {
    let cancelled = false;
    if (!loadedOnce.current) setLoading(true);
    setError(null);
    async function load() {
      try {
        const [sources, favorites] = await Promise.all([listPlaylists(), listFavorites()]);
        // Limit simultaneous IPC requests for libraries with many sources. One source failing to
        // list (a malformed row, a transient lock) must not blank every other source's channels:
        // `allSettled` keeps what loaded and only the failed sources are left out, with a notice
        // naming them, rather than the whole library looking empty and "disconnected."
        const loaded: Channel[] = [];
        const failedNames: string[] = [];
        for (let index = 0; index < sources.length; index += 4) {
          const batch = sources.slice(index, index + 4);
          const results = await Promise.allSettled(batch.map((source) => listChannels(source.id)));
          if (cancelled) return;
          results.forEach((result, position) => {
            if (result.status === "fulfilled") {
              loaded.push(...result.value);
            } else {
              failedNames.push(batch[position].name);
            }
          });
        }
        if (cancelled) return;
        setPlaylists(sources);
        setAllChannels(loaded);
        setFavoriteIds(new Set(favorites));
        setSelectedPlaylistId((current) => current !== null && sources.some((source) => source.id === current) ? current : null);
        loadedOnce.current = true;
        if (failedNames.length) {
          onNoticeRef.current?.(tn("app.library.sourcesFailedToLoad", failedNames.length, { names: failedNames.join(", ") }));
        }
      } catch (failure) {
        if (!cancelled) setError(errorMessage(failure));
      } finally {
        if (!cancelled) setLoading(false);
      }
    }
    void load();
    return () => { cancelled = true; };
  }, [refreshTrigger, revision]);

  const sourceChannels = useMemo(() => selectedPlaylistId === null ? allChannels : allChannels.filter((channel) => channel.playlist_id === selectedPlaylistId), [allChannels, selectedPlaylistId]);
  const groups = useMemo(() => [...new Set(sourceChannels.map((channel) => channel.group_title || "Other"))].sort(), [sourceChannels]);
  // This hook powers the Sources management page too, where every channel must stay visible
  // and manageable regardless of the "hide unavailable"/"show adult content" preferences —
  // those only gate what the Home/Discover/My List pages show, not what you can see and edit here.
  const channels = useMemo(() => filterChannels(sourceChannels, searchQuery, selectedGroup, false, true).filter((channel) => !favoritesOnly || favoriteIds.has(channel.id)), [sourceChannels, searchQuery, selectedGroup, favoritesOnly, favoriteIds]);
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
      const active = await toggleFavorite(id);
      setFavoriteIds((previous) => {
        const next = new Set(previous);
        if (active) next.add(id); else next.delete(id);
        return next;
      });
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

  return { playlists, selectedPlaylistId, selectPlaylist, allChannels, channels, groups, selectedGroup, setSelectedGroup,
    searchQuery, setSearchQuery, favoritesOnly, setFavoritesOnly, favoriteIds, loading, probing, refreshing, error,
    loadPlaylists: reload, loadChannels: reload, handleProbeVisible, handleToggleFavorite, handleSetCategoryOverride,
    handleDeletePlaylist, handleRefreshPlaylist };
}

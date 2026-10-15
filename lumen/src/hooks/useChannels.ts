import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import { listPlaylists, listChannels, probeUrls, toggleFavorite, listFavorites, deletePlaylist, errorMessage } from "../lib/tauri";
import type { Channel, Playlist } from "../lib/types";
import { filterChannels } from "../lib/discovery";

export function useChannels(refreshTrigger = 0) {
  const [playlists, setPlaylists] = useState<Playlist[]>([]);
  const [selectedPlaylistId, setSelectedPlaylistId] = useState<number | null>(null);
  const [allChannels, setAllChannels] = useState<Channel[]>([]);
  const [selectedGroup, setSelectedGroup] = useState("all");
  const [searchQuery, setSearchQuery] = useState("");
  const [favoritesOnly, setFavoritesOnly] = useState(false);
  const [favoriteIds, setFavoriteIds] = useState<Set<number>>(new Set());
  const [loading, setLoading] = useState(true);
  const [probing, setProbing] = useState(false);
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
        // Limit simultaneous IPC requests for libraries with many sources.
        const loaded: Channel[] = [];
        for (let index = 0; index < sources.length; index += 4) {
          const batch = await Promise.all(sources.slice(index, index + 4).map((source) => listChannels(source.id)));
          if (cancelled) return;
          loaded.push(...batch.flat());
        }
        if (cancelled) return;
        setPlaylists(sources);
        setAllChannels(loaded);
        setFavoriteIds(new Set(favorites));
        setSelectedPlaylistId((current) => current !== null && sources.some((source) => source.id === current) ? current : null);
        loadedOnce.current = true;
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
  const channels = useMemo(() => filterChannels(sourceChannels, searchQuery, selectedGroup, false).filter((channel) => !favoritesOnly || favoriteIds.has(channel.id)), [sourceChannels, searchQuery, selectedGroup, favoritesOnly, favoriteIds]);
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

  const handleDeletePlaylist = useCallback(async (id: number) => {
    try {
      await deletePlaylist(id);
      reload();
    } catch (failure) {
      setError(errorMessage(failure));
      throw failure;
    }
  }, [reload]);

  return { playlists, selectedPlaylistId, selectPlaylist, allChannels, channels, groups, selectedGroup, setSelectedGroup,
    searchQuery, setSearchQuery, favoritesOnly, setFavoritesOnly, favoriteIds, loading, probing, error,
    loadPlaylists: reload, loadChannels: reload, handleProbeVisible, handleToggleFavorite, handleDeletePlaylist };
}

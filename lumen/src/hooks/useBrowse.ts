import { useCallback, useEffect, useRef, useState } from "react";
import { channelFacetsReady, listBrowseFacets, listBrowsePage, errorMessage } from "../lib/tauri";
import { buildFacetsFromCounts, type FacetSelection, type Facets } from "../lib/discovery";
import type { Channel } from "../lib/types";

const PAGE_SIZE = 500;

interface BrowseOptions {
  /** Only the Movies/Series/Live pages use the server-side facet rail. */
  enabled: boolean;
  playlistId: number | null;
  kind: "movie" | "series" | "live" | null;
  selection: FacetSelection;
  hideUnavailable: boolean;
  showAdult: boolean;
  refreshTrigger: number;
  /** Lets the normal library cache open details and episode lists for paged cards. */
  onChannels?: (channels: Channel[]) => void;
}

export function useBrowse(options: BrowseOptions) {
  const {
    enabled, playlistId, kind, selection, hideUnavailable, showAdult, refreshTrigger,
  } = options;
  const onChannelsRef = useRef(options.onChannels);
  onChannelsRef.current = options.onChannels;

  const [ready, setReady] = useState<boolean | null>(null);
  const [facets, setFacets] = useState<Facets | null>(null);
  const [channels, setChannels] = useState<Channel[]>([]);
  const [total, setTotal] = useState(0);
  const [nextAfterName, setNextAfterName] = useState<string | null>(null);
  const [nextAfterId, setNextAfterId] = useState<number | null>(null);
  const [hasMore, setHasMore] = useState(false);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const requestId = useRef(0);
  const loadingMoreRef = useRef(false);

  const load = useCallback(async () => {
    if (!enabled) {
      requestId.current += 1;
      setReady(null);
      setFacets(null);
      setChannels([]);
      setTotal(0);
      setHasMore(false);
      setLoading(false);
      return;
    }
    const request = ++requestId.current;
    setLoading(true);
    setError(null);
    try {
      const counts = await listBrowseFacets(
        playlistId,
        kind,
        selection.group,
        selection.genre,
        selection.year,
        hideUnavailable,
        showAdult,
      );
      if (request !== requestId.current) return;
      setReady(counts.ready);
      if (!counts.ready) {
        setFacets(null);
        setChannels([]);
        setTotal(0);
        setHasMore(false);
        return;
      }
      setFacets(buildFacetsFromCounts(counts));
      const page = await listBrowsePage(
        playlistId,
        kind,
        selection.group,
        selection.genre,
        selection.year,
        hideUnavailable,
        showAdult,
        null,
        null,
        PAGE_SIZE,
      );
      if (request !== requestId.current) return;
      setChannels(page.channels);
      setTotal(page.total);
      setNextAfterName(page.next_after_name);
      setNextAfterId(page.next_after_id);
      setHasMore(page.has_more);
      onChannelsRef.current?.(page.channels);
    } catch (failure) {
      if (request === requestId.current) setError(errorMessage(failure));
    } finally {
      if (request === requestId.current) setLoading(false);
    }
  }, [
    enabled, playlistId, kind, selection.group, selection.genre, selection.year,
    hideUnavailable, showAdult,
  ]);

  useEffect(() => {
    void load();
  }, [load, refreshTrigger]);

  // The startup backfill can finish while the app is open. Those checks are a cheap readiness
  // read until it does, so refreshing the rails within a minute does not re-scan the library.
  useEffect(() => {
    if (!enabled || ready !== false) return;
    const timer = window.setInterval(() => { void load(); }, 60_000);
    return () => { window.clearInterval(timer); };
  }, [enabled, ready, load]);

  const loadMore = useCallback(async () => {
    if (!enabled || nextAfterName === null || nextAfterId === null || !hasMore || loadingMoreRef.current) return;
    loadingMoreRef.current = true;
    const request = requestId.current;
    setLoadingMore(true);
    try {
      const page = await listBrowsePage(
        playlistId,
        kind,
        selection.group,
        selection.genre,
        selection.year,
        hideUnavailable,
        showAdult,
        nextAfterName,
        nextAfterId,
        PAGE_SIZE,
      );
      if (request !== requestId.current) return;
      setChannels((current) => {
        const byId = new Map(current.map((channel) => [channel.id, channel]));
        for (const channel of page.channels) byId.set(channel.id, channel);
        return [...byId.values()];
      });
      setTotal(page.total);
      setNextAfterName(page.next_after_name);
      setNextAfterId(page.next_after_id);
      setHasMore(page.has_more);
      onChannelsRef.current?.(page.channels);
    } catch (failure) {
      if (request === requestId.current) setError(errorMessage(failure));
    } finally {
      loadingMoreRef.current = false;
      setLoadingMore(false);
    }
  }, [
    enabled, nextAfterName, nextAfterId, hasMore, playlistId, kind, selection.group,
    selection.genre, selection.year, hideUnavailable, showAdult,
  ]);

  return {
    ready, facets, channels, total, hasMore, loading, loadingMore, error, loadMore,
    reload: load,
  };
}

/** Public readiness probe for pages that need to know when the index is built. */
export async function facetsAreReady() {
  return channelFacetsReady();
}

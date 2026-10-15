import { useState, useEffect, useCallback, useRef } from "react";
import {
  playUrl,
  pause as pauseCmd,
  seek as seekCmd,
  setVolume as setVolumeCmd,
  setPlayerTrack as setPlayerTrackCmd,
  setPlayerQuality as setPlayerQualityCmd,
  stop as stopCmd,
  onPlayerState,
  saveProgress,
  PlayerState,
  Channel,
  isTauriRuntime,
  errorMessage,
} from "../lib/tauri";
import { DEFAULT_PLAYER_STATE } from "../lib/types";
import type { PlayerOptions, PlayerTrackKind } from "../lib/types";

export function usePlayer() {
  const [playerState, setPlayerState] = useState<PlayerState>(DEFAULT_PLAYER_STATE);
  const browserTrackHandler = useRef<((kind: PlayerTrackKind, id: string) => void) | null>(null);
  const browserQualityHandler = useRef<((id: string) => void) | null>(null);
  const [currentTitle, setCurrentTitle] = useState<string | null>(null);
  const currentTitleRef = useRef(currentTitle);
  currentTitleRef.current = currentTitle;
  const [currentUrl, setCurrentUrl] = useState<string | null>(null);
  const [currentChannelId, setCurrentChannelId] = useState<number | undefined>(undefined);
  const [volume, setVolumeState] = useState<number>(80);
  const [muted, setMuted] = useState<boolean>(false);
  const [seekRequest, setSeekRequest] = useState<{ time: number; seq: number } | null>(null);
  const prevVolumeRef = useRef<number>(80);
  const progressRef = useRef({ currentUrl, currentChannelId, playerState });
  progressRef.current = { currentUrl, currentChannelId, playerState };
  const headersRef = useRef<Record<string, string> | undefined>(undefined);
  const reportError = useCallback((failure: unknown) => {
    setPlayerState((previous) => ({ ...previous, error: errorMessage(failure) }));
  }, []);

  useEffect(() => {
    const unlistenPromise = onPlayerState((state) => {
      setPlayerState(state);
      if (state.title && !currentTitleRef.current) {
        currentTitleRef.current = state.title;
        setCurrentTitle(state.title);
      }
    });
    unlistenPromise.catch(reportError);
    return () => {
      unlistenPromise.then((fn) => fn()).catch(reportError);
    };
  }, [reportError]);

  // Periodically persist playback progress for resumable media
  useEffect(() => {
    if (!currentUrl || playerState.paused || playerState.duration <= 0) return;
    const timer = setInterval(() => {
      const current = progressRef.current;
      if (current.currentUrl && current.playerState.time > 1 && current.playerState.duration > 0) {
        saveProgress(current.currentUrl, current.playerState.time, current.playerState.duration, current.currentChannelId).catch(reportError);
      }
    }, 5000);
    return () => clearInterval(timer);
  }, [currentUrl, currentChannelId, playerState.paused, playerState.duration, reportError]);

  const playChannel = useCallback(async (channel: Channel) => {
    let parsedHeaders: Record<string, string> | undefined;
    if (channel.extra_headers && channel.extra_headers !== "{}") {
      try {
        parsedHeaders = JSON.parse(channel.extra_headers);
      } catch {
        parsedHeaders = undefined;
      }
    }
    await playUrl(channel.url, parsedHeaders, channel.id);
    headersRef.current = parsedHeaders;
    setCurrentTitle(channel.name);
    setCurrentUrl(channel.url);
    setCurrentChannelId(channel.id);
    setPlayerState((prev) => ({
      ...prev,
      paused: false,
      time: 0,
      duration: 0,
      buffering: true,
      title: channel.name,
      error: undefined,
    }));

  }, []);

  const playMedia = useCallback(
    async (url: string, title?: string, headers?: Record<string, string>) => {
      await playUrl(url, headers);
      headersRef.current = headers;
      const label = title || url.split("/").pop() || url;
      setCurrentTitle(label);
      setCurrentUrl(url);
      setCurrentChannelId(undefined);
      setPlayerState((prev) => ({
        ...prev,
        paused: false,
        time: 0,
        duration: 0,
        buffering: true,
        title: label,
        error: undefined,
      }));

    },
    []
  );

  const togglePause = useCallback(async () => {
    if (isTauriRuntime()) await pauseCmd();
    setPlayerState((prev) => ({ ...prev, paused: !prev.paused }));
  }, []);

  const seekTo = useCallback(async (seconds: number) => {
    const target = Math.max(0, seconds);
    setSeekRequest((prev) => ({ time: target, seq: (prev?.seq ?? 0) + 1 }));
    setPlayerState((prev) => ({ ...prev, time: target }));
    if (isTauriRuntime()) await seekCmd(target);
  }, []);

  const seekRelative = useCallback(
    async (delta: number) => {
      const target = Math.max(0, playerState.time + delta);
      setSeekRequest((prev) => ({ time: target, seq: (prev?.seq ?? 0) + 1 }));
      setPlayerState((prev) => ({ ...prev, time: target }));
      if (isTauriRuntime()) await seekCmd(target);
    },
    [playerState.time]
  );

  const changeVolume = useCallback(async (newVol: number) => {
    const clamped = Math.max(0, Math.min(100, newVol));
    setVolumeState(clamped);
    setMuted(clamped === 0);
    if (clamped > 0) {
      prevVolumeRef.current = clamped;
    }
    if (isTauriRuntime()) await setVolumeCmd(clamped);
  }, []);

  const toggleMute = useCallback(async () => {
    if (muted || volume === 0) {
      const restore = prevVolumeRef.current > 0 ? prevVolumeRef.current : 80;
      await changeVolume(restore);
    } else {
      prevVolumeRef.current = volume;
      setMuted(true);
      setVolumeState(0);
      if (isTauriRuntime()) await setVolumeCmd(0);
    }
  }, [muted, volume, changeVolume]);

  const stopPlayback = useCallback(async () => {
    if (currentUrl && playerState.time > 1 && playerState.duration > 0) {
      await saveProgress(
        currentUrl,
        playerState.time,
        playerState.duration,
        currentChannelId
      ).catch(reportError);
    }
    await stopCmd();
    headersRef.current = undefined;
    setCurrentTitle(null);
    setCurrentUrl(null);
    setCurrentChannelId(undefined);
    setPlayerState(DEFAULT_PLAYER_STATE);
  }, [currentUrl, currentChannelId, playerState.time, playerState.duration, reportError]);

  const handleVideoTimeUpdate = useCallback((time: number, duration: number) => {
    setPlayerState((prev) => ({ ...prev, time, duration }));
  }, []);

  const handleVideoPlayState = useCallback((paused: boolean) => {
    setPlayerState((prev) => ({ ...prev, paused }));
  }, []);

  const handleVideoBuffering = useCallback((buffering: boolean) => {
    setPlayerState((prev) => ({ ...prev, buffering }));
  }, []);

  const handleVideoMutedChange = useCallback((nextMuted: boolean) => {
    setMuted(nextMuted);
  }, []);

  const handleVideoError = useCallback((error: string | undefined) => {
    setPlayerState((prev) => ({ ...prev, error }));
  }, []);

  const handleVideoOptions = useCallback((options: PlayerOptions) => {
    setPlayerState((previous) => ({ ...previous, ...options }));
  }, []);

  const registerBrowserControls = useCallback((
    track: (kind: PlayerTrackKind, id: string) => void,
    quality: (id: string) => void,
  ) => {
    browserTrackHandler.current = track;
    browserQualityHandler.current = quality;
    return () => {
      if (browserTrackHandler.current === track) browserTrackHandler.current = null;
      if (browserQualityHandler.current === quality) browserQualityHandler.current = null;
    };
  }, []);

  const selectTrack = useCallback(async (kind: PlayerTrackKind, id: string) => {
    if (isTauriRuntime()) {
      await setPlayerTrackCmd(kind, id);
    } else if (browserTrackHandler.current) {
      browserTrackHandler.current(kind, id);
    } else {
      throw new Error("Track selection is unavailable for this playback source");
    }
  }, []);

  const selectQuality = useCallback(async (id: string) => {
    if (isTauriRuntime()) {
      await setPlayerQualityCmd(id);
      setPlayerState((previous) => ({ ...previous, quality: id }));
    } else if (browserQualityHandler.current) {
      browserQualityHandler.current(id);
    } else {
      throw new Error("Quality selection is unavailable for this playback source");
    }
  }, []);

  return {
    playerState,
    currentTitle,
    currentUrl,
    currentChannelId,
    volume,
    muted,
    seekRequest,
    playChannel,
    playMedia,
    togglePause,
    seekTo,
    seekRelative,
    changeVolume,
    toggleMute,
    stopPlayback,
    handleVideoTimeUpdate,
    handleVideoPlayState,
    handleVideoBuffering,
    handleVideoMutedChange,
    handleVideoError,
    handleVideoOptions,
    registerBrowserControls,
    selectTrack,
    selectQuality,
  };
}

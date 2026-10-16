import { useState, useEffect, useCallback, useRef } from "react";
import {
  playUrl,
  nextFallbackSource,
  pause as pauseCmd,
  seek as seekCmd,
  setVolume as setVolumeCmd,
  setPlayerTrack as setPlayerTrackCmd,
  setPlayerQuality as setPlayerQualityCmd,
  getPlayerPrefs,
  setPlayerPrefs,
  setSubtitleDelay,
  setSpeed,
  addSubtitleFile,
  stop as stopCmd,
  onPlayerState,
  saveProgress,
  PlayerState,
  Channel,
  errorMessage,
} from "../lib/tauri";
import { DEFAULT_PLAYER_STATE } from "../lib/types";
import { clampScale, SUB_DELAY_STEP, SUB_SCALE_STEP } from "../lib/languages";
import { t } from "../i18n/index.ts";
import type { PlayerTrackKind } from "../lib/types";

interface PlayerOptions {
  /** Told when playback moves to another source by itself, so the user is not left guessing. */
  onNotice?: (message: string) => void;
}

export function usePlayer(options: PlayerOptions = {}) {
  const onNoticeRef = useRef(options.onNotice);
  onNoticeRef.current = options.onNotice;
  const [playerState, setPlayerState] = useState<PlayerState>(DEFAULT_PLAYER_STATE);
  const [currentTitle, setCurrentTitle] = useState<string | null>(null);
  const currentTitleRef = useRef(currentTitle);
  currentTitleRef.current = currentTitle;
  const [currentUrl, setCurrentUrl] = useState<string | null>(null);
  const [currentChannelId, setCurrentChannelId] = useState<number | undefined>(undefined);
  const [volume, setVolumeState] = useState<number>(80);
  const [muted, setMuted] = useState<boolean>(false);
  const prevVolumeRef = useRef<number>(80);
  const progressRef = useRef({ currentUrl, currentChannelId, playerState });
  progressRef.current = { currentUrl, currentChannelId, playerState };
  const headersRef = useRef<Record<string, string> | undefined>(undefined);
  // The stream URL whose failure already triggered a fallback, so one failure moves on once.
  const fallbackFromRef = useRef<string | null>(null);
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

  // When the current stream fails, move to the next ranked source of the same title, if any.
  useEffect(() => {
    const failedUrl = progressRef.current.currentUrl;
    if (!playerState.error || !failedUrl || fallbackFromRef.current === failedUrl) return;
    fallbackFromRef.current = failedUrl;
    nextFallbackSource(failedUrl)
      .then(async (next) => {
        if (!next) return;
        await playUrl(next.url, next.headers ?? undefined, next.channel_id ?? undefined);
        onNoticeRef.current?.(t("lib.notice.sourceFailed", { title: currentTitleRef.current ?? t("lib.notice.thisTitle") }));
        headersRef.current = next.headers ?? undefined;
        setCurrentUrl(next.url);
        setCurrentChannelId(next.channel_id ?? undefined);
        setPlayerState((prev) => ({
          ...prev,
          paused: false,
          time: 0,
          duration: 0,
          buffering: true,
          error: undefined,
        }));
      })
      .catch(reportError);
  }, [playerState.error, reportError]);

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

  // The backend sets the volume before the file loads; this only brings the UI state in line.
  const applyStartVolume = useCallback((startVolume?: number) => {
    if (startVolume === undefined) return;
    const clamped = Math.max(0, Math.min(100, startVolume));
    setVolumeState(clamped);
    setMuted(clamped === 0);
    if (clamped > 0) prevVolumeRef.current = clamped;
  }, []);

  const playChannel = useCallback(async (channel: Channel, startVolume?: number) => {
    let parsedHeaders: Record<string, string> | undefined;
    if (channel.extra_headers && channel.extra_headers !== "{}") {
      try {
        parsedHeaders = JSON.parse(channel.extra_headers);
      } catch {
        parsedHeaders = undefined;
      }
    }
    fallbackFromRef.current = null;
    await playUrl(channel.url, parsedHeaders, channel.id, startVolume);
    applyStartVolume(startVolume);
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

  }, [applyStartVolume]);

  const playMedia = useCallback(
    async (url: string, title?: string, headers?: Record<string, string>, startVolume?: number, audioUrl?: string, channelId?: number) => {
      fallbackFromRef.current = null;
      await playUrl(url, headers, channelId, startVolume, audioUrl);
      applyStartVolume(startVolume);
      headersRef.current = headers;
      const label = title || url.split("/").pop() || url;
      setCurrentTitle(label);
      setCurrentUrl(url);
      setCurrentChannelId(channelId);
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
    [applyStartVolume]
  );

  const togglePause = useCallback(async () => {
    await pauseCmd();
    setPlayerState((prev) => ({ ...prev, paused: !prev.paused }));
  }, []);

  const seekTo = useCallback(async (seconds: number) => {
    const target = Math.max(0, seconds);
    setPlayerState((prev) => ({ ...prev, time: target }));
    await seekCmd(target);
  }, []);

  const seekRelative = useCallback(
    async (delta: number) => {
      const target = Math.max(0, playerState.time + delta);
        setPlayerState((prev) => ({ ...prev, time: target }));
      await seekCmd(target);
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
    await setVolumeCmd(clamped);
  }, []);

  const toggleMute = useCallback(async () => {
    if (muted || volume === 0) {
      const restore = prevVolumeRef.current > 0 ? prevVolumeRef.current : 80;
      await changeVolume(restore);
    } else {
      prevVolumeRef.current = volume;
      setMuted(true);
      setVolumeState(0);
      await setVolumeCmd(0);
    }
  }, [muted, volume, changeVolume]);

  const stopPlayback = useCallback(async () => {
    if (currentUrl && playerState.time > 1 && playerState.duration > 0) {
      await saveProgress(
        currentUrl,
        playerState.time,
        playerState.duration,
        currentChannelId,
        true
      ).catch(reportError);
    }
    await stopCmd();
    headersRef.current = undefined;
    setCurrentTitle(null);
    setCurrentUrl(null);
    setCurrentChannelId(undefined);
    setPlayerState(DEFAULT_PLAYER_STATE);
  }, [currentUrl, currentChannelId, playerState.time, playerState.duration, reportError]);

  const selectTrack = useCallback(async (kind: PlayerTrackKind, id: string) => {
    await setPlayerTrackCmd(kind, id);
  }, []);

  // The subtitle delay belongs to the file that is playing: it starts again at zero for the next one.
  const [subtitleDelay, setSubtitleDelayState] = useState(0);
  useEffect(() => { setSubtitleDelayState(0); }, [currentUrl]);

  // Speed also belongs to one file: the player starts the next one at normal speed.
  const [speed, setSpeedState] = useState(1);
  useEffect(() => { setSpeedState(1); }, [currentUrl]);
  const changeSpeed = useCallback(async (next: number) => {
    setSpeedState(await setSpeed(next));
  }, []);

  const changeSubtitleDelay = useCallback(async (direction: 1 | -1) => {
    const applied = await setSubtitleDelay(Math.round((subtitleDelay + direction * SUB_DELAY_STEP) * 10) / 10);
    setSubtitleDelayState(applied);
  }, [subtitleDelay]);

  // Size is a profile preference, so a change here is saved, not just applied to this file.
  const changeSubtitleScale = useCallback(async (direction: 1 | -1) => {
    const prefs = await getPlayerPrefs();
    await setPlayerPrefs({ ...prefs, subScale: clampScale(prefs.subScale + direction * SUB_SCALE_STEP) });
  }, []);

  const addSubtitle = useCallback(async (path: string) => { await addSubtitleFile(path); }, []);

  const selectQuality = useCallback(async (id: string) => {
    await setPlayerQualityCmd(id);
    setPlayerState((previous) => ({ ...previous, quality: id }));
  }, []);

  return {
    playerState,
    currentTitle,
    currentUrl,
    currentChannelId,
    volume,
    muted,
    playChannel,
    playMedia,
    togglePause,
    seekTo,
    seekRelative,
    changeVolume,
    toggleMute,
    stopPlayback,
    selectTrack,
    selectQuality,
    speed,
    changeSpeed,
    subtitleDelay,
    changeSubtitleDelay,
    changeSubtitleScale,
    addSubtitle,
  };
}

import { useEffect, useRef } from "react";
import Hls from "hls.js";
import { resolvePlayableSrc } from "../lib/tauri";

interface VideoStageProps {
  url: string | null;
  title: string | null;
  paused: boolean;
  volume: number;
  muted: boolean;
  seekRequest: { time: number; seq: number } | null;
  onTimeUpdate: (time: number, duration: number) => void;
  onPlayStateChange: (paused: boolean) => void;
  onBufferingChange: (buffering: boolean) => void;
  onMutedChange: (muted: boolean) => void;
  onError: (err: string | undefined) => void;
}

export function VideoStage({
  url,
  title,
  paused,
  volume,
  muted,
  seekRequest,
  onTimeUpdate,
  onPlayStateChange,
  onBufferingChange,
  onMutedChange,
  onError,
}: VideoStageProps) {
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const hlsRef = useRef<Hls | null>(null);
  const retriesRef = useRef(0);
  const callbacksRef = useRef({ onError, onBufferingChange, onMutedChange });
  callbacksRef.current = { onError, onBufferingChange, onMutedChange };

  // Load stream when URL changes
  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;

    if (hlsRef.current) {
      hlsRef.current.destroy();
      hlsRef.current = null;
    }

    if (!url) {
      video.removeAttribute("src");
      video.load();
      return;
    }

    retriesRef.current = 0;
    callbacksRef.current.onError(undefined);
    callbacksRef.current.onBufferingChange(true);

    const src = resolvePlayableSrc(url);
    // A malformed percent escape in a provider URL must not abort this effect before the
    // browser gets a chance to load it. Decode for encoded extensions when possible, but keep
    // detection safe for arbitrary user-supplied URLs.
    let decodedSrc = src;
    try {
      decodedSrc = decodeURIComponent(src);
    } catch {
      // The raw source can still be checked below.
    }
    const isHls =
      decodedSrc.toLowerCase().includes(".m3u8") ||
      src.toLowerCase().includes("mpegurl");

    if (isHls && Hls.isSupported()) {
      const hls = new Hls({
        enableWorker: true,
        lowLatencyMode: true,
      });
      hlsRef.current = hls;
      hls.loadSource(src);
      hls.attachMedia(video);

      hls.on(Hls.Events.MANIFEST_PARSED, () => {
        onBufferingChange(false);
        video.play().catch(() => {
          // If browser blocks unmuted autoplay, start muted so video frames render immediately
          video.muted = true;
          callbacksRef.current.onMutedChange(true);
          video.play().catch(() => { callbacksRef.current.onBufferingChange(false); callbacksRef.current.onError("Playback could not start. Press Play to retry."); });
        });
      });

      hls.on(Hls.Events.ERROR, (_event, data) => {
        if (data.fatal) {
          switch (data.type) {
            case Hls.ErrorTypes.NETWORK_ERROR:
            case Hls.ErrorTypes.MEDIA_ERROR:
              if (retriesRef.current++ < 2) {
                if (data.type === Hls.ErrorTypes.NETWORK_ERROR) hls.startLoad();
                else hls.recoverMediaError();
              } else {
                callbacksRef.current.onBufferingChange(false);
                callbacksRef.current.onError(`Unable to play this stream: ${data.details}`);
                hls.destroy();
              }
              break;
            default:
              onBufferingChange(false);
              onError(`Stream error: ${data.details}`);
              hls.destroy();
              break;
          }
        }
      });
    } else {
      video.src = src;
      video.load();
      video.play().catch(() => {
        video.muted = true;
        callbacksRef.current.onMutedChange(true);
        video.play().catch(() => {
          callbacksRef.current.onBufferingChange(false);
          callbacksRef.current.onError("Playback could not start. Press Play to retry.");
        });
      });
    }

    return () => {
      video.pause();
      video.removeAttribute("src");
      video.load();
      if (hlsRef.current) {
        hlsRef.current.destroy();
        hlsRef.current = null;
      }
    };
  }, [url]);

  // Sync play/pause state
  useEffect(() => {
    const video = videoRef.current;
    if (!video || !url) return;
    if (paused && !video.paused) {
      video.pause();
    } else if (!paused && video.paused) {
      video.play().catch(() => { callbacksRef.current.onBufferingChange(false); callbacksRef.current.onError("Playback could not start. Press Play to retry."); });
    }
  }, [paused, url]);

  // Sync volume & mute
  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;
    video.volume = Math.max(0, Math.min(1, volume / 100));
    video.muted = muted || volume === 0;
  }, [volume, muted]);

  // Sync explicit user seek
  useEffect(() => {
    const video = videoRef.current;
    if (!video || !seekRequest) return;
    if (Number.isFinite(seekRequest.time)) {
      video.currentTime = Math.max(0, seekRequest.time);
    }
  }, [seekRequest]);

  return (
    <div className="video-stage">
      {url ? (
        <video
          ref={videoRef}
          className="video-element"
          playsInline
          controls={false}
          onTimeUpdate={(e) => {
            const v = e.currentTarget;
            const dur =
              Number.isFinite(v.duration) && v.duration > 0 ? v.duration : 0;
            onTimeUpdate(v.currentTime || 0, dur);
          }}
          onPlay={() => {
            onPlayStateChange(false);
            onBufferingChange(false);
          }}
          onPause={() => onPlayStateChange(true)}
          onWaiting={() => onBufferingChange(true)}
          onPlaying={() => onBufferingChange(false)}
          onCanPlay={() => onBufferingChange(false)}
          onError={() => {
            onBufferingChange(false);
            onError("Failed to play media stream");
          }}
        />
      ) : (
        <div className="stage-placeholder">
          <div className="stage-icon">▶</div>
          <h3>{title || "Select a Channel or Media File"}</h3>
          <p>
            Click any channel on the left, browse Local Files, or click + Add
            Source to import an M3U playlist or extract a stream URL.
          </p>
        </div>
      )}
    </div>
  );
}

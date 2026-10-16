import { useEffect, useRef, useState } from "react";
import { castControl, castState, errorMessage } from "../lib/tauri";
import type { CastState } from "../lib/types";
import { formatClock } from "../lib/local";
import { t } from "../i18n/index.ts";
import { Icon } from "./Icon";

const POLL_MS = 2000;

function stateLabel(state: string): string {
  switch (state) {
    case "PLAYING": return t("cast.bar.state.PLAYING");
    case "PAUSED_PLAYBACK": return t("cast.bar.state.PAUSED_PLAYBACK");
    case "STOPPED": return t("cast.bar.state.STOPPED");
    case "TRANSITIONING": return t("cast.bar.state.TRANSITIONING");
    default: return t("cast.bar.state.connected");
  }
}

interface CastBarProps {
  cast: CastState;
  /** Seconds the skip buttons jump. */
  seekStep: number;
  onEnded: () => void;
  onError: (message: string) => void;
}

/** Controls for the cast in progress: shown while a TV plays what was sent. */
export function CastBar({ cast, seekStep, onEnded, onError }: CastBarProps) {
  const [current, setCurrent] = useState<CastState>(cast);
  // `onEnded` is typically a fresh closure every render of the parent (it closes over parent
  // state); reading it through a ref instead of listing it as an effect dependency keeps an
  // unrelated re-render from tearing down and restarting the poll before it ever fires.
  const onEndedRef = useRef(onEnded);
  onEndedRef.current = onEnded;

  useEffect(() => {
    let active = true;
    setCurrent(cast);
    const timer = window.setInterval(() => {
      castState()
        .then((next) => {
          if (!active) return;
          if (next) setCurrent(next); else onEndedRef.current();
        })
        .catch(() => { /* the device may be briefly busy; the next poll tries again */ });
    }, POLL_MS);
    return () => { active = false; window.clearInterval(timer); };
  }, [cast]);

  const control = async (action: "play" | "pause" | "stop" | "seek", position?: number) => {
    try {
      await castControl(action, position);
      if (action === "stop") onEnded();
    } catch (failure) {
      onError(errorMessage(failure));
    }
  };

  const status = current.status;
  const playing = status?.state === "PLAYING";
  const position = status?.position ?? 0;
  const skip = (delta: number) => void control("seek", Math.max(0, position + delta));

  return (
    <div className="cast-bar" role="region" aria-label={t("cast.bar.label")}>
      <div className="cast-bar-text">
        <strong>{t("cast.bar.to", { title: current.title, device: current.device })}</strong>
        <span className="muted-note">
          {status ? stateLabel(status.state) : t("cast.bar.state.connected")}
          {status && status.duration > 0 ? ` · ${t("cast.bar.position", { time: formatClock(position), total: formatClock(status.duration) })}` : ""}
        </span>
      </div>
      <div className="cast-bar-buttons">
        <button className="transport-button transport-text-button" aria-label={t("transport.back", { seconds: seekStep })} title={t("transport.back", { seconds: seekStep })} onClick={() => skip(-seekStep)}>−{seekStep}</button>
        <button className="transport-button" aria-label={playing ? t("cast.bar.pause") : t("cast.bar.play")} title={playing ? t("cast.bar.pause") : t("cast.bar.play")} onClick={() => void control(playing ? "pause" : "play")}>
          <Icon name={playing ? "pause" : "play"} size={18} />
        </button>
        <button className="transport-button transport-text-button" aria-label={t("transport.forward", { seconds: seekStep })} title={t("transport.forward", { seconds: seekStep })} onClick={() => skip(seekStep)}>+{seekStep}</button>
        <button className="transport-button" aria-label={t("cast.bar.stop")} title={t("cast.bar.stop")} onClick={() => void control("stop")}>
          <Icon name="stop" size={18} />
        </button>
      </div>
    </div>
  );
}

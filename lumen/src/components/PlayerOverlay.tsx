import { useCallback, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { Icon } from "./Icon";
import "./PlayerOverlay.css";

const CONTROLS_IDLE_MS = 2500;
// Long enough to tell a double-click from two single clicks, short enough to feel immediate.
const CLICK_DELAY_MS = 220;
const WAKE_EVENTS = ["pointermove", "pointerdown", "keydown"] as const;

interface PlayerOverlayProps {
  title: string;
  paused: boolean;
  /** Hidden players keep their media element mounted, so playback continues in the background. */
  hidden: boolean;
  onBack: () => void;
  onTogglePause: () => void;
  onToggleFullscreen: () => void;
  /** Set only when the series has a following episode; the button is hidden otherwise. */
  nextEpisodeTitle?: string;
  onNextEpisode?: () => void;
  /** True while the stream is loading or refilling. The spinner ignores the idle state, so it stays visible after the controls fade. */
  buffering: boolean;
  controls: ReactNode;
  children: ReactNode;
}

export function PlayerOverlay({
  title,
  paused,
  hidden,
  onBack,
  onTogglePause,
  onToggleFullscreen,
  nextEpisodeTitle,
  onNextEpisode,
  buffering,
  controls,
  children,
}: PlayerOverlayProps) {
  const [controlsAwake, setControlsAwake] = useState(true);
  const hideTimer = useRef<number | undefined>(undefined);
  const clickTimer = useRef<number | undefined>(undefined);

  const wake = useCallback(() => {
    setControlsAwake(true);
    window.clearTimeout(hideTimer.current);
    hideTimer.current = window.setTimeout(() => setControlsAwake(false), CONTROLS_IDLE_MS);
  }, []);

  // Showing the player again should reveal the controls, even if they idled out while hidden.
  useEffect(() => {
    if (!hidden) wake();
  }, [hidden, wake]);

  useEffect(() => {
    WAKE_EVENTS.forEach((name) => window.addEventListener(name, wake));
    return () => {
      WAKE_EVENTS.forEach((name) => window.removeEventListener(name, wake));
      window.clearTimeout(hideTimer.current);
      window.clearTimeout(clickTimer.current);
    };
  }, [wake]);

  // Controls stay visible while paused, as they do in streaming apps.
  const showControls = controlsAwake || paused;

  return (
    <section
      className={`player-overlay${hidden ? " is-hidden" : ""}${showControls ? "" : " controls-idle"}`}
      aria-label="Video player"
    >
      <div
        className="player-stage"
        onClick={(event) => {
          // A double-click also fires two click events; only the first one should count.
          if (event.detail > 1) return;
          window.clearTimeout(clickTimer.current);
          clickTimer.current = window.setTimeout(onTogglePause, CLICK_DELAY_MS);
        }}
        onDoubleClick={() => {
          window.clearTimeout(clickTimer.current);
          onToggleFullscreen();
        }}
      >
        {children}
      </div>

      {buffering && (
        <div className="player-buffering" role="status" aria-label="Buffering">
          <span className="player-spinner" aria-hidden="true" />
          <p className="player-buffering-label">Buffering…</p>
        </div>
      )}

      <header className="player-topbar">
        <button type="button" className="player-back" onClick={onBack} aria-label="Back to browsing">
          <Icon name="arrow" size={22} />
          <span>Back</span>
        </button>
        <h2 className="player-title">{title}</h2>
        {onNextEpisode && (
          <button
            type="button"
            className="player-next"
            onClick={onNextEpisode}
            title={nextEpisodeTitle ? `Next: ${nextEpisodeTitle}` : undefined}
          >
            <span>Next episode</span>
            <Icon name="chevron" size={18} />
          </button>
        )}
      </header>

      <div className="player-controls">{controls}</div>
    </section>
  );
}

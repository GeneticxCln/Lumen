import { Icon } from "./Icon";
import { PlayerState } from "../lib/tauri";

interface TransportProps {
  state: PlayerState;
  volume: number;
  muted: boolean;
  onPlayPause: () => void;
  onStop: () => void;
  onSeek: (seconds: number) => void;
  onSeekRelative: (delta: number) => void;
  onVolumeChange: (vol: number) => void;
  onToggleMute: () => void;
}

export function Transport({
  state,
  volume,
  muted,
  onPlayPause,
  onStop,
  onSeek,
  onSeekRelative,
  onVolumeChange,
  onToggleMute,
}: TransportProps) {
  const formatTime = (seconds: number) => {
    const safe = Math.max(0, Math.floor(seconds || 0));
    const hrs = Math.floor(safe / 3600);
    const mins = Math.floor((safe % 3600) / 60);
    const secs = safe % 60;
    if (hrs > 0) {
      return `${hrs}:${mins.toString().padStart(2, "0")}:${secs
        .toString()
        .padStart(2, "0")}`;
    }
    return `${mins}:${secs.toString().padStart(2, "0")}`;
  };

  const displayTime = Math.min(Math.max(0, state.time), state.duration || 0);

  return (
    <div className="transport">
      <div className="transport-controls">
        <button
          onClick={onStop}
          disabled={state.paused && state.time === 0 && !state.title}
          title="Stop"
        >
          <Icon name="stop" size={17} />
        </button>
        <button
          onClick={() => onSeekRelative(-10)}
          title="Rewind 10s (Left Arrow)"
        >
          ⏪
        </button>
        <button
          onClick={onPlayPause}
          title={state.paused ? "Play (Space)" : "Pause (Space)"}
          className="play-pause-btn"
        >
          <Icon name={state.paused ? "play" : "pause"} size={22} />
        </button>
        <button
          onClick={() => onSeekRelative(10)}
          title="Forward 10s (Right Arrow)"
        >
          ⏩
        </button>
      </div>

      <div className="progress-bar">
        <span className="time-current">{formatTime(displayTime)}</span>
        <input
          type="range"
          min="0"
          max={state.duration > 0 ? state.duration : 100}
          step="0.5"
          value={displayTime}
          aria-label="Playback position"
          onChange={(e) => onSeek(Number(e.target.value))}
          disabled={state.duration <= 0}
        />
        <span className="time-total">
          {state.duration > 0 ? formatTime(state.duration) : "LIVE"}
        </span>
      </div>

      <div className="volume-control">
        <button
          type="button"
          className="mute-btn"
          onClick={onToggleMute}
          title="Mute / Unmute (M)"
        >
          <Icon name={muted || volume === 0 ? "mute" : "volume"} size={20} />
        </button>
        <input
          type="range"
          min="0"
          max="100"
          aria-label="Volume"
          value={muted ? 0 : volume}
          onChange={(e) => onVolumeChange(parseInt(e.target.value, 10))}
        />
        <span className="volume-value">{muted ? 0 : volume}%</span>
      </div>

      <div className="keyboard-hints">
        <span>Space: Play/Pause</span>
        <span>←/→: Seek ±10s</span>
        <span>9/0: Vol ±5%</span>
        <span>M: Mute</span>
        <span>F: Fullscreen</span>
      </div>

      {state.buffering && <div className="buffering-indicator" role="status">Buffering stream...</div>}
      {state.error && <div className="error-message" role="alert">{state.error}</div>}
    </div>
  );
}

import { Icon } from "./Icon";
import type { PlayerState } from "../lib/types";
import { PLAYER_SEEK_STEP_SECONDS } from "../lib/playerKeys";

interface TransportProps {
  state: PlayerState;
  volume: number;
  muted: boolean;
  isFullscreen: boolean;
  onPlayPause: () => void;
  onStop: () => void;
  onSeek: (seconds: number) => void;
  onSeekRelative: (delta: number) => void;
  onVolumeChange: (vol: number) => void;
  onToggleMute: () => void;
  onToggleFullscreen: () => void;
  onSelectTrack: (kind: "audio" | "subtitle", id: string) => void;
  onSelectQuality: (id: string) => void;
}

export function Transport({
  state,
  volume,
  muted,
  isFullscreen,
  onPlayPause,
  onStop,
  onSeek,
  onSeekRelative,
  onVolumeChange,
  onToggleMute,
  onToggleFullscreen,
  onSelectTrack,
  onSelectQuality,
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
  const step = PLAYER_SEEK_STEP_SECONDS;

  return (
    <div className="transport">
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

      <div className="transport-row">
        <div className="transport-group">
          <button
            type="button"
            onClick={onPlayPause}
            title={state.paused ? "Play (Space)" : "Pause (Space)"}
            aria-label={state.paused ? "Play" : "Pause"}
            className="transport-button play-pause-btn"
          >
            <Icon name={state.paused ? "play" : "pause"} size={22} />
          </button>
          <button
            type="button"
            className="transport-button transport-text-button"
            onClick={() => onSeekRelative(-step)}
            title={`Back ${step} seconds (Left Arrow)`}
            aria-label={`Back ${step} seconds`}
          >
            −{step}
          </button>
          <button
            type="button"
            className="transport-button transport-text-button"
            onClick={() => onSeekRelative(step)}
            title={`Forward ${step} seconds (Right Arrow)`}
            aria-label={`Forward ${step} seconds`}
          >
            +{step}
          </button>
          <div className="volume-control">
            <button
              type="button"
              className="transport-button"
              onClick={onToggleMute}
              title="Mute / Unmute (M)"
              aria-label={muted || volume === 0 ? "Unmute" : "Mute"}
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
        </div>

        <div className="transport-group transport-group-end">
          <div className="player-track-controls" aria-label="Stream tracks and quality">
            <label className="player-option">
              <span>Audio</span>
              <select aria-label="Audio track" value={state.audioTrack} onChange={(event) => onSelectTrack("audio", event.target.value)} disabled={!state.tracks.some((track) => track.kind === "audio")}>
                <option value="auto">Auto</option>
                {state.tracks.filter((track) => track.kind === "audio").map((track) => <option key={track.id} value={track.id}>{track.label}</option>)}
              </select>
            </label>
            <label className="player-option">
              <span>Subtitles</span>
              <select aria-label="Subtitle track" value={state.subtitleTrack} onChange={(event) => onSelectTrack("subtitle", event.target.value)} disabled={!state.tracks.some((track) => track.kind === "subtitle")}>
                <option value="off">Off</option>
                {state.tracks.filter((track) => track.kind === "subtitle").map((track) => <option key={track.id} value={track.id}>{track.label}</option>)}
              </select>
            </label>
            <label className="player-option">
              <span>Quality</span>
              <select aria-label="Playback quality" value={state.quality} onChange={(event) => onSelectQuality(event.target.value)} disabled={!state.qualityLevels.length} title={state.qualityHelp}>
                {state.qualityLevels.length ? state.qualityLevels.map((quality) => <option key={quality.id} value={quality.id}>{quality.label}</option>) : <option value="">Unavailable</option>}
              </select>
            </label>
          </div>
          <button
            type="button"
            className="transport-button"
            onClick={onStop}
            disabled={state.paused && state.time === 0 && !state.title}
            title="Stop playback"
            aria-label="Stop playback"
          >
            <Icon name="stop" size={17} />
          </button>
          <button
            type="button"
            className="transport-button"
            onClick={onToggleFullscreen}
            title={isFullscreen ? "Exit fullscreen (F)" : "Fullscreen (F)"}
            aria-label={isFullscreen ? "Exit fullscreen" : "Fullscreen"}
          >
            <Icon name={isFullscreen ? "collapse" : "expand"} size={20} />
          </button>
        </div>
      </div>

      {state.qualityLevels.length === 0 && state.qualityHelp && <small className="player-option-hint">{state.qualityHelp}</small>}
      {state.buffering && <div className="buffering-indicator" role="status">Buffering stream...</div>}
      {state.error && <div className="error-message" role="alert">{state.error}</div>}
    </div>
  );
}

import { Icon } from "./Icon";
import type { PlayerState } from "../lib/types";
import { formatDelay, SPEEDS } from "../lib/languages";
import { t } from "../i18n/index.ts";

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
  subtitleDelay: number;
  onSubtitleDelay: (direction: 1 | -1) => void;
  onSubtitleScale: (direction: 1 | -1) => void;
  onAddSubtitle: () => void;
  onFindSubtitles: () => void;
  /** Seconds the skip buttons jump. */
  seekStep: number;
  speed: number;
  onSpeed: (speed: number) => void;
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
  subtitleDelay,
  onSubtitleDelay,
  onSubtitleScale,
  onAddSubtitle,
  onFindSubtitles,
  seekStep,
  speed,
  onSpeed,
}: TransportProps) {
  const hasSubtitles = state.tracks.some((track) => track.kind === "subtitle");
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
  const step = seekStep;

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
          aria-label={t("transport.position")}
          onChange={(e) => onSeek(Number(e.target.value))}
          disabled={state.duration <= 0}
        />
        <span className="time-total">
          {state.duration > 0 ? formatTime(state.duration) : t("transport.live")}
        </span>
      </div>

      <div className="transport-row">
        <div className="transport-group">
          <button
            type="button"
            onClick={onPlayPause}
            title={state.paused ? t("transport.playHint") : t("transport.pauseHint")}
            aria-label={state.paused ? t("transport.play") : t("transport.pause")}
            className="transport-button play-pause-btn"
          >
            <Icon name={state.paused ? "play" : "pause"} size={22} />
          </button>
          <button
            type="button"
            className="transport-button transport-text-button"
            onClick={() => onSeekRelative(-step)}
            title={t("transport.backHint", { seconds: step })}
            aria-label={t("transport.back", { seconds: step })}
          >
            −{step}
          </button>
          <button
            type="button"
            className="transport-button transport-text-button"
            onClick={() => onSeekRelative(step)}
            title={t("transport.forwardHint", { seconds: step })}
            aria-label={t("transport.forward", { seconds: step })}
          >
            +{step}
          </button>
          <div className="volume-control">
            <button
              type="button"
              className="transport-button"
              onClick={onToggleMute}
              title={t("transport.muteHint")}
              aria-label={muted || volume === 0 ? t("transport.unmute") : t("transport.mute")}
            >
              <Icon name={muted || volume === 0 ? "mute" : "volume"} size={20} />
            </button>
            <input
              type="range"
              min="0"
              max="100"
              aria-label={t("transport.volume")}
              value={muted ? 0 : volume}
              onChange={(e) => onVolumeChange(parseInt(e.target.value, 10))}
            />
            <span className="volume-value">{muted ? 0 : volume}%</span>
          </div>
        </div>

        <div className="transport-group transport-group-end">
          <div className="player-track-controls" aria-label={t("transport.tracks")}>
            <label className="player-option">
              <span>{t("transport.audio")}</span>
              <select aria-label={t("transport.audioTrack")} value={state.audioTrack} onChange={(event) => onSelectTrack("audio", event.target.value)} disabled={!state.tracks.some((track) => track.kind === "audio")}>
                <option value="auto">{t("transport.auto")}</option>
                {state.tracks.filter((track) => track.kind === "audio").map((track) => <option key={track.id} value={track.id}>{track.label}</option>)}
              </select>
            </label>
            <label className="player-option">
              <span>{t("transport.subtitles")}</span>
              <select aria-label={t("transport.subtitleTrack")} value={state.subtitleTrack} onChange={(event) => onSelectTrack("subtitle", event.target.value)} disabled={!state.tracks.some((track) => track.kind === "subtitle")}>
                <option value="off">{t("common.off")}</option>
                {state.tracks.filter((track) => track.kind === "subtitle").map((track) => <option key={track.id} value={track.id}>{track.label}</option>)}
              </select>
            </label>
            <div className="player-option subtitle-tools" role="group" aria-label={t("transport.subtitleAdjustments")}>
              <span>{t("transport.subtitleTools")}</span>
              <div>
                <button className="icon-button" disabled={!hasSubtitles} aria-label={t("transport.subtitlesEarlier")} title={t("transport.subtitlesEarlierHint")} onClick={() => onSubtitleDelay(-1)}>−</button>
                <output aria-label={t("transport.subtitleDelay")}>{formatDelay(subtitleDelay)}</output>
                <button className="icon-button" disabled={!hasSubtitles} aria-label={t("transport.subtitlesLater")} title={t("transport.subtitlesLaterHint")} onClick={() => onSubtitleDelay(1)}>+</button>
                <button className="icon-button" disabled={!hasSubtitles} aria-label={t("transport.subtitlesSmaller")} title={t("transport.subtitlesSmaller")} onClick={() => onSubtitleScale(-1)}>{t("transport.subtitlesSmallerGlyph")}</button>
                <button className="icon-button" disabled={!hasSubtitles} aria-label={t("transport.subtitlesLarger")} title={t("transport.subtitlesLarger")} onClick={() => onSubtitleScale(1)}>{t("transport.subtitlesLargerGlyph")}</button>
                <button className="icon-button" aria-label={t("transport.addSubtitle")} title={t("transport.addSubtitle")} onClick={onAddSubtitle}>{t("transport.addSubtitleGlyph")}</button>
                <button className="icon-button" aria-label={t("transport.findSubtitles")} title={t("transport.findSubtitles")} onClick={onFindSubtitles}>{t("transport.findSubtitlesGlyph")}</button>
              </div>
            </div>
            <label className="player-option">
              <span>{t("transport.speed")}</span>
              <select aria-label={t("transport.speedLabel")} value={speed} onChange={(event) => onSpeed(Number(event.target.value))}>
                {(SPEEDS.includes(speed) ? SPEEDS : [...SPEEDS, speed].sort((a, b) => a - b)).map((value) => <option key={value} value={value}>{t("transport.speedValue", { speed: value })}</option>)}
              </select>
            </label>
            <label className="player-option">
              <span>{t("transport.quality")}</span>
              <select aria-label={t("transport.qualityLabel")} value={state.quality} onChange={(event) => onSelectQuality(event.target.value)} disabled={!state.qualityLevels.length} title={state.qualityHelp}>
                {state.qualityLevels.length ? state.qualityLevels.map((quality) => <option key={quality.id} value={quality.id}>{quality.label}</option>) : <option value="">{t("transport.qualityUnavailable")}</option>}
              </select>
            </label>
          </div>
          <button
            type="button"
            className="transport-button"
            onClick={onStop}
            disabled={state.paused && state.time === 0 && !state.title}
            title={t("transport.stop")}
            aria-label={t("transport.stop")}
          >
            <Icon name="stop" size={17} />
          </button>
          <button
            type="button"
            className="transport-button"
            onClick={onToggleFullscreen}
            title={isFullscreen ? t("transport.exitFullscreenHint") : t("transport.fullscreenHint")}
            aria-label={isFullscreen ? t("transport.exitFullscreen") : t("transport.fullscreen")}
          >
            <Icon name={isFullscreen ? "collapse" : "expand"} size={20} />
          </button>
        </div>
      </div>

      {state.qualityLevels.length === 0 && state.qualityHelp && <small className="player-option-hint">{state.qualityHelp}</small>}
      {/* The centered spinner in PlayerOverlay owns the buffering announcement, so the controls must not repeat it. */}
      {state.error && <div className="error-message" role="alert">{state.error}</div>}
    </div>
  );
}

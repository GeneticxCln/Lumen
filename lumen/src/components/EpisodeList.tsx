import { buildSeasons, seriesIdOf } from "../lib/series";
import type { EpisodeMark } from "../lib/series";
import type { Channel, HistoryEntry } from "../lib/types";
import { Icon } from "./Icon";

interface EpisodeListProps {
  current: Channel;
  channels: Channel[];
  marks: EpisodeMark[];
  history: HistoryEntry[];
  busy: boolean;
  onPlay: (channel: Channel) => void;
  onMark: (seriesKey: string, season: number, episodes: number[], watched: boolean) => void;
}

/** The seasons of a series from the library, with a watched toggle per episode and per season. */
export function EpisodeList({ current, channels, marks, history, busy, onPlay, onMark }: EpisodeListProps) {
  const series = seriesIdOf(current);
  const seasons = buildSeasons(current, channels, marks, history);
  if (!series || seasons.length === 0) return null;
  const currentSeason = seasons.find((season) => season.episodes.some((row) => row.channel.id === current.id))?.season;

  return <div className="episode-list">
    <h3 className="settings-section-title">Episodes</h3>
    {seasons.map((season) => {
      const allWatched = season.watchedCount === season.episodes.length;
      return <details key={season.season} open={season.season === currentSeason || seasons.length === 1}>
        <summary>
          Season {season.season} <span className="muted-note">{season.watchedCount} of {season.episodes.length} watched</span>
        </summary>
        <div className="settings-actions">
          <button
            className="secondary-button"
            disabled={busy}
            onClick={() => onMark(series, season.season, season.episodes.map((row) => row.episode), !allWatched)}
          >
            {allWatched ? "Mark season unwatched" : "Mark season watched"}
          </button>
        </div>
        <ul className="episode-rows">
          {season.episodes.map((row) => <li key={row.episode} className={row.watched ? "is-watched" : ""}>
            <button
              className={`icon-button circle ${row.watched ? "is-saved" : ""}`}
              aria-pressed={row.watched}
              aria-label={`${row.watched ? "Mark unwatched" : "Mark watched"}: ${row.channel.name}`}
              disabled={busy}
              onClick={() => onMark(series, row.season, [row.episode], !row.watched)}
            >
              <Icon name={row.watched ? "check" : "plus"} size={16} />
            </button>
            <span className="episode-name">E{String(row.episode).padStart(2, "0")} · {row.channel.name}</span>
            <button className="icon-button" aria-label={`Play ${row.channel.name}`} disabled={busy} onClick={() => onPlay(row.channel)}>
              <Icon name="play" size={16} />
            </button>
          </li>)}
        </ul>
      </details>;
    })}
  </div>;
}

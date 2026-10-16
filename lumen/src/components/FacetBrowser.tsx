import { useEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import type { Channel } from "../lib/types";
import type { Facets, FacetSelection, FacetValue } from "../lib/discovery";
import { ALL_FACETS, artworkCategory, genresOf, groupLabel, yearOf } from "../lib/discovery";
import { TMDB_POSTER_SIZE, tmdbImage } from "../lib/tauri";
import { Icon } from "./Icon";
import { t, tn } from "../i18n/index.ts";

interface FacetBrowserProps {
  channels: Channel[];
  facets: Facets;
  selection: FacetSelection;
  onSelectionChange: (selection: FacetSelection) => void;
  favorites: Set<number>;
  onPlay: (channel: Channel) => void;
  onDetails: (channel: Channel) => void;
  onFavorite: (id: number) => void;
  compact: boolean;
  /** Label for the "nothing to show" state, so each page can explain itself. */
  emptyMessage: string;
}

function FacetRail({
  label, values, selected, onSelect, hint,
}: {
  label: string;
  values: FacetValue[];
  selected: string;
  onSelect: (value: string) => void;
  hint?: string;
}) {
  return <div className="facet-rail">
    <div className="facet-rail-head">
      <span className="facet-label">{label}</span>
      {hint && <span className="facet-hint">{hint}</span>}
    </div>
    <div className="facet-values" role="group" aria-label={label}>
      {values.map((value) => {
        const active = value.value === selected;
        // A facet value with no reachable titles is not worth offering.
        const dead = value.count === 0 && value.value !== ALL_FACETS;
        return <button
          key={value.value}
          className={`facet-chip ${active ? "active" : ""}`}
          aria-pressed={active}
          disabled={dead}
          onClick={() => onSelect(active ? ALL_FACETS : value.value)}
          title={tn("facets.chipTitle", value.count, { label: value.label })}
          aria-label={tn("facets.chipLabel", value.count, { label: value.label })}
        >
          {/* The count is decorative here; the button's own label announces it. */}
          <span aria-hidden="true">{value.label}</span>
          <span className="facet-count" aria-hidden="true">{value.count}</span>
        </button>;
      })}
    </div>
  </div>;
}

function PosterCard({
  channel, width, favorite, onPlay, onDetails, onFavorite,
}: {
  channel: Channel;
  width: number;
  favorite: boolean;
  onPlay: (channel: Channel) => void;
  onDetails: (channel: Channel) => void;
  onFavorite: (id: number) => void;
}) {
  const [posterFailed, setPosterFailed] = useState(false);
  const [logoFailed, setLogoFailed] = useState(false);
  const poster = tmdbImage(channel.poster_path, TMDB_POSTER_SIZE);
  const year = yearOf(channel);
  const { names, source } = genresOf(channel);
  const genreText = names.length ? names.slice(0, 2).join(", ") : groupLabel(channel.group_title);

  return <article className="poster-card" style={{ width }}>
    <button className="poster-art-button" aria-label={t("episodes.play", { name: channel.name })} onClick={() => onPlay(channel)}>
      {poster && !posterFailed
        ? <img className="poster-image" src={poster} alt="" loading="lazy" onError={() => setPosterFailed(true)} />
        : <span className={`channel-art poster-fallback art-${artworkCategory(channel.group_title)}`}>
            <img className="landscape-art" src="/artwork/landscape.svg" alt="" aria-hidden="true" />
            {channel.tvg_logo && !logoFailed && <img className="source-logo" src={channel.tvg_logo} alt="" loading="lazy" referrerPolicy="no-referrer" onError={() => setLogoFailed(true)} />}
            <span className="poster-fallback-title">{channel.name}</span>
          </span>}
      <span className="card-play"><Icon name="play" size={24} /></span>
      {/* Marks which titles carry real TMDB metadata rather than inferred genres. */}
      {source === "tmdb" && <span className="poster-source-tag">{t("facets.tmdbTag")}</span>}
      {channel.last_error && <span className="card-badge unavailable">{t("transport.qualityUnavailable")}</span>}
    </button>
    <div className="poster-meta">
      <div className="poster-meta-text">
        <strong title={channel.name}>{channel.name}</strong>
        <span>{year ? t("facets.genreYear", { genre: genreText, year }) : genreText}</span>
      </div>
      <div className="poster-actions">
        <button
          className={`icon-button circle ${favorite ? "is-saved" : ""}`}
          aria-label={favorite ? t("shelf.removeFromList", { name: channel.name }) : t("shelf.addToList", { name: channel.name })}
          aria-pressed={favorite}
          onClick={() => onFavorite(channel.id)}
        >
          <Icon name={favorite ? "check" : "plus"} size={16} />
        </button>
        <button className="icon-button circle" aria-label={t("shelf.details", { name: channel.name })} onClick={() => onDetails(channel)}>
          <Icon name="info" size={16} />
        </button>
      </div>
    </div>
  </article>;
}

export function FacetBrowser({
  channels, facets, selection, onSelectionChange, favorites, onPlay, onDetails, onFavorite, compact, emptyMessage,
}: FacetBrowserProps) {
  const trackRef = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);

  // The grid's column count follows the container, so measure rather than guess.
  useEffect(() => {
    const element = trackRef.current;
    if (!element) return;
    setWidth(element.clientWidth);
    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) setWidth(entry.contentRect.width);
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  const gap = 18;
  const minCard = compact ? 148 : 176;
  const columns = Math.max(2, Math.floor((width || 900) / minCard));
  const cardWidth = Math.max(120, ((width || 900) - gap * (columns - 1)) / columns);
  const rowHeight = cardWidth * 1.5 + 62;
  const rows = Math.ceil(channels.length / columns);

  const virtualizer = useVirtualizer({
    count: rows,
    getScrollElement: () => trackRef.current,
    estimateSize: () => rowHeight,
    overscan: 2,
  });

  const set = (dimension: keyof FacetSelection, value: string) => onSelectionChange({ ...selection, [dimension]: value });

  return <>
    <div className="facet-browser">
      <aside className="facet-sidebar" aria-label={t("facets.filterTitles")}>
        <FacetRail label={t("facets.group")} values={facets.groups} selected={selection.group} onSelect={(value) => set("group", value)} />
        <FacetRail
          label={t("facets.genre")}
          values={facets.genres}
          selected={selection.genre}
          onSelect={(value) => set("genre", value)}
          hint={facets.hasTmdbGenres ? t("facets.genreHintTmdb") : t("facets.genreHintSource")}
        />
        <FacetRail label={t("facets.year")} values={facets.years} selected={selection.year} onSelect={(value) => set("year", value)} hint={t("facets.yearHint")} />
      </aside>

      <section className="facet-results" aria-label={t("facets.browseTitles")}>
        <div className="facet-results-heading">
          <div><h2>{t("facets.titles")}</h2><p>{t("facets.titlesNote")}</p></div>
          {facets.enriched > 0 && <span className="facet-enriched-count">{t("facets.enriched", { count: facets.enriched })}</span>}
        </div>
        {channels.length === 0
          ? <div className="empty-state">
              <div className="empty-symbol"><Icon name="grid" size={32} /></div>
              <h2>{emptyMessage}</h2>
              <p>{t("facets.emptyHint")}</p>
            </div>
          : <div className="poster-grid-viewport" ref={trackRef} tabIndex={0} aria-label={t("facets.gridLabel", { count: channels.length })}>
              <div style={{ height: virtualizer.getTotalSize(), width: "100%", position: "relative" }}>
                {virtualizer.getVirtualItems().map((row) => {
                  const start = row.index * columns;
                  return <div
                    className="poster-grid-row"
                    key={row.key}
                    style={{ position: "absolute", top: 0, left: 0, width: "100%", height: row.size, transform: `translateY(${row.start}px)` }}
                  >
                    {channels.slice(start, start + columns).map((channel) => <PosterCard
                      key={channel.id}
                      channel={channel}
                      width={cardWidth}
                      favorite={favorites.has(channel.id)}
                      onPlay={onPlay}
                      onDetails={onDetails}
                      onFavorite={onFavorite}
                    />)}
                  </div>;
                })}
              </div>
            </div>}
      </section>
    </div>
  </>;
}

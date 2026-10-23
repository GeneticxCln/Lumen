import { useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import type { BrowseTitle, Channel } from "../lib/types";
import { artworkCategory, browseCaption } from "../lib/discovery";
import { remainingLabel } from "../lib/board";
import type { ContinueItem } from "../lib/board";
import { TMDB_BACKDROP_SIZE, tmdbImage } from "../lib/tauri";
import { RemoteImage } from "./RemoteImage";
import { Icon } from "./Icon";
import { t } from "../i18n/index.ts";

export function ChannelArtwork({ channel, hero = false }: { channel: Channel; hero?: boolean }) {
  const [artFailed, setArtFailed] = useState(false);
  const [logoFailed, setLogoFailed] = useState(false);
  // These frames are 16:9 like a TMDB backdrop, so a backdrop drops straight in.
  // A 2:3 poster would be cropped badly here — the poster grid uses those instead.
  const backdrop = tmdbImage(channel.backdrop_path, TMDB_BACKDROP_SIZE);
  return <div className={`channel-art art-${artworkCategory(channel.group_title)} ${hero ? "hero-art" : ""}`}>
    {backdrop && !artFailed
      ? <RemoteImage className="landscape-art tmdb-art" src={backdrop} alt="" loading="lazy" decoding="async" onError={() => setArtFailed(true)} />
      : <img className="landscape-art" src="/artwork/landscape.svg" alt="" aria-hidden="true" />}
    {channel.tvg_logo && !logoFailed && <RemoteImage className="source-logo" src={channel.tvg_logo} alt="" loading="lazy" decoding="async" referrerPolicy="no-referrer" onError={() => setLogoFailed(true)} />}
    {!hero && <span className="art-title">{channel.name}</span>}
  </div>;
}

function BrowseArtwork({ title }: { title: BrowseTitle }) {
  const [artFailed, setArtFailed] = useState(false);
  const backdrop = tmdbImage(title.backdrop_path ?? undefined, TMDB_BACKDROP_SIZE);
  return <div className="channel-art">
    {backdrop && !artFailed
      ? <RemoteImage className="landscape-art tmdb-art" src={backdrop} alt="" loading="lazy" decoding="async" onError={() => setArtFailed(true)} />
      : <img className="landscape-art" src="/artwork/landscape.svg" alt="" aria-hidden="true" />}
    <span className="art-title">{title.name}</span>
  </div>;
}

interface BrowseShelfProps {
  title: string;
  subtitle?: string;
  titles: BrowseTitle[];
  compact: boolean;
}

/**
 * TMDB titles that are not necessarily in the library. Cards show artwork and details only:
 * there is no play, My List or details control, because nothing here can be played.
 */
export function BrowseShelf({ title, subtitle, titles, compact }: BrowseShelfProps) {
  const ref = useRef<HTMLDivElement>(null);
  const width = compact ? 224 : 280;
  if (!titles.length) return null;
  const scroll = (direction: 1 | -1) => ref.current?.scrollBy({
    left: direction * ref.current.clientWidth * .8,
    behavior: document.documentElement.classList.contains("reduce-motion") || window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "instant" : "smooth",
  });
  return <section className="media-shelf" aria-label={title}>
    <div className="shelf-heading"><div><h2>{title}<span className="shelf-count">{titles.length}</span></h2>{subtitle && <p>{subtitle}</p>}</div>
      <div className="shelf-arrows"><button className="icon-button" aria-label={t("shelf.scrollLeft", { title })} onClick={() => scroll(-1)}><Icon name="chevron" style={{ transform: "rotate(180deg)" }} /></button><button className="icon-button" aria-label={t("shelf.scrollRight", { title })} onClick={() => scroll(1)}><Icon name="chevron" /></button></div>
    </div>
    <div className="shelf-track" ref={ref} tabIndex={0} aria-label={t("shelf.titlesLabel", { title })}>
      <div style={{ display: "flex", gap: 12, width: "max-content", paddingRight: 12 }}>
        {titles.map((item) => <article className="media-card" key={`${item.tmdb_type}-${item.tmdb_id}`} style={{ width: width - 12 }}>
          <div className="card-art-button">
            <BrowseArtwork title={item} />
            <span className="card-badge">{t("shelf.notInLibrary")}</span>
          </div>
          <div className="card-bottom"><div><h3 className="sr-only">{item.name}</h3><p>{browseCaption(item)}</p></div></div>
        </article>)}
      </div>
    </div>
  </section>;
}

interface ShelfProps {
  title: string;
  subtitle?: string;
  channels: Channel[];
  favorites: Set<number>;
  onPlay: (channel: Channel) => void;
  onDetails: (channel: Channel) => void;
  onFavorite: (id: number) => void;
  compact: boolean;
  ranked?: boolean;
}
export function MediaShelf({ title, subtitle, channels, favorites, onPlay, onDetails, onFavorite, compact, ranked = false }: ShelfProps) {
  const ref = useRef<HTMLDivElement>(null);
  const width = compact ? 224 : 280;
  const virtualizer = useVirtualizer({ count: channels.length, horizontal: true, getScrollElement: () => ref.current, estimateSize: () => width, overscan: 3 });
  if (!channels.length) return null;
  return <section className="media-shelf" aria-label={title}>
    <div className="shelf-heading"><div><h2>{title}<span className="shelf-count">{channels.length}</span></h2>{subtitle && <p>{subtitle}</p>}</div>
      <div className="shelf-arrows"><button className="icon-button" aria-label={t("shelf.scrollLeft", { title })} onClick={() => ref.current?.scrollBy({ left: -(ref.current.clientWidth * .8), behavior: document.documentElement.classList.contains("reduce-motion") || window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "instant" : "smooth" })}><Icon name="chevron" style={{ transform: "rotate(180deg)" }} /></button><button className="icon-button" aria-label={t("shelf.scrollRight", { title })} onClick={() => ref.current?.scrollBy({ left: ref.current.clientWidth * .8, behavior: document.documentElement.classList.contains("reduce-motion") || window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "instant" : "smooth" })}><Icon name="chevron" /></button></div>
    </div>
    <div className="shelf-track" ref={ref} tabIndex={0} aria-label={t("shelf.titlesLabel", { title })}>
      <div style={{ width: virtualizer.getTotalSize(), height: compact ? 190 : 223, position: "relative" }}>
        {virtualizer.getVirtualItems().map((item) => {
          const channel = channels[item.index];
          const favorite = favorites.has(channel.id);
          return <article className={`media-card ${ranked ? "ranked-card" : ""}`} key={channel.id} style={{ position: "absolute", top: 0, left: 0, width: width - 12, transform: `translateX(${item.start}px)` }}>
            <button className="card-art-button" aria-label={t("episodes.play", { name: channel.name })} onClick={() => onPlay(channel)}>
              <ChannelArtwork key={channel.id} channel={channel} />
              <span className="card-play"><Icon name="play" size={24} /></span>
              <span className={`card-badge ${channel.last_error ? "unavailable" : ""}`}>{channel.last_error ? t("transport.qualityUnavailable") : channel.last_ok ? t("shelf.lastCheckedOk") : t("shelf.stream")}</span>
              {ranked && <span className="card-rank">{item.index + 1}</span>}
            </button>
            <div className="card-bottom"><div><h3 className="sr-only">{channel.name}</h3><p>{channel.group_title || t("shelf.yourLibrary")}</p></div><button className={`icon-button circle ${favorite ? "is-saved" : ""}`} aria-label={favorite ? t("shelf.removeFromList", { name: channel.name }) : t("shelf.addToList", { name: channel.name })} aria-pressed={favorite} onClick={() => onFavorite(channel.id)}><Icon name={favorite ? "check" : "plus"} size={17} /></button><button className="icon-button circle" aria-label={t("shelf.details", { name: channel.name })} onClick={() => onDetails(channel)}><Icon name="info" size={17} /></button></div>
          </article>;
        })}
      </div>
    </div>
  </section>;
}

interface ContinueShelfProps {
  items: ContinueItem[];
  compact: boolean;
  onPlayChannel: (channel: Channel) => void;
  onPlayFile: (path: string, name: string) => void;
}

/** Things you started and can pick up where you stopped: library streams and files on this computer. */
export function ContinueShelf({ items, compact, onPlayChannel, onPlayFile }: ContinueShelfProps) {
  const ref = useRef<HTMLDivElement>(null);
  const width = compact ? 224 : 280;
  if (!items.length) return null;
  const title = t("shelf.continue.title");
  return <section className="media-shelf" aria-label={title}>
    <div className="shelf-heading"><div><h2>{title}<span className="shelf-count">{items.length}</span></h2><p>{t("shelf.continue.subtitle")}</p></div></div>
    <div className="shelf-track" ref={ref} tabIndex={0} aria-label={t("shelf.titlesLabel", { title })}>
      <div style={{ display: "flex", gap: 12, width: "max-content", paddingRight: 12 }}>
        {items.map((item) => <article className="media-card" key={item.key} style={{ width: width - 12 }}>
          <button className="card-art-button" aria-label={t("shelf.continue.resume", { title: item.title })} onClick={() => item.channel ? onPlayChannel(item.channel) : onPlayFile(item.path!, item.title)}>
            {item.channel
              ? <ChannelArtwork channel={item.channel} />
              : <div className="channel-art"><img className="landscape-art" src="/artwork/landscape.svg" alt="" aria-hidden="true" /><span className="art-title">{item.title}</span></div>}
            <span className="card-play"><Icon name="play" size={24} /></span>
            <span className="card-progress" role="progressbar" aria-label={t("shelf.continue.progress", { title: item.title })} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(item.progress * 100)}><span style={{ width: `${Math.round(item.progress * 100)}%` }} /></span>
          </button>
          <div className="card-bottom"><div><h3 className="sr-only">{item.title}</h3><p>{item.path ? t("shelf.continue.onThisComputer", { remaining: remainingLabel(item.remainingSeconds) }) : remainingLabel(item.remainingSeconds)}</p></div></div>
        </article>)}
      </div>
    </div>
  </section>;
}

export interface UpcomingCard {
  key: string;
  name: string;
  /** "S03E04 airs tomorrow". */
  text: string;
  kind: "upcoming" | "aired";
  /** A library entry of the series, when there is one. */
  channel?: Channel;
}

/** Series you follow with an episode on the way or just out. Opens the series' details. */
export function UpcomingShelf({ items, compact, onDetails }: { items: UpcomingCard[]; compact: boolean; onDetails: (channel: Channel) => void }) {
  const width = compact ? 224 : 280;
  if (!items.length) return null;
  const title = t("shelf.upcoming.title");
  return <section className="media-shelf" aria-label={title}>
    <div className="shelf-heading"><div><h2>{title}<span className="shelf-count">{items.length}</span></h2><p>{t("shelf.upcoming.subtitle")}</p></div></div>
    <div className="shelf-track" tabIndex={0} aria-label={t("shelf.seriesLabel", { title })}>
      <div style={{ display: "flex", gap: 12, width: "max-content", paddingRight: 12 }}>
        {items.map((item) => <article className="media-card" key={item.key} style={{ width: width - 12 }}>
          <button className="card-art-button" aria-label={t("shelf.upcoming.card", { name: item.name, text: item.text })} disabled={!item.channel} onClick={() => item.channel && onDetails(item.channel)}>
            {item.channel
              ? <ChannelArtwork channel={item.channel} />
              : <div className="channel-art"><img className="landscape-art" src="/artwork/landscape.svg" alt="" aria-hidden="true" /><span className="art-title">{item.name}</span></div>}
            <span className="card-badge">{item.kind === "aired" ? t("shelf.upcoming.newEpisode") : t("shelf.upcoming.upcoming")}</span>
          </button>
          <div className="card-bottom"><div><h3 className="sr-only">{item.name}</h3><p>{item.text}</p></div></div>
        </article>)}
      </div>
    </div>
  </section>;
}

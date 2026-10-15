import { useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import type { Channel } from "../lib/types";
import { artworkCategory } from "../lib/discovery";
import { TMDB_BACKDROP_SIZE, tmdbImage } from "../lib/tauri";
import { Icon } from "./Icon";

export function ChannelArtwork({ channel, hero = false }: { channel: Channel; hero?: boolean }) {
  const [artFailed, setArtFailed] = useState(false);
  const [logoFailed, setLogoFailed] = useState(false);
  // These frames are 16:9 like a TMDB backdrop, so a backdrop drops straight in.
  // A 2:3 poster would be cropped badly here — the poster grid uses those instead.
  const backdrop = tmdbImage(channel.backdrop_path, TMDB_BACKDROP_SIZE);
  return <div className={`channel-art art-${artworkCategory(channel.group_title)} ${hero ? "hero-art" : ""}`}>
    {backdrop && !artFailed
      ? <img className="landscape-art tmdb-art" src={backdrop} alt="" loading="lazy" onError={() => setArtFailed(true)} />
      : <img className="landscape-art" src="/artwork/landscape.svg" alt="" aria-hidden="true" />}
    {channel.tvg_logo && !logoFailed && <img className="source-logo" src={channel.tvg_logo} alt="" loading="lazy" referrerPolicy="no-referrer" onError={() => setLogoFailed(true)} />}
    {!hero && <span className="art-title">{channel.name}</span>}
  </div>;
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
      <div className="shelf-arrows"><button className="icon-button" aria-label={`Scroll ${title} left`} onClick={() => ref.current?.scrollBy({ left: -(ref.current.clientWidth * .8), behavior: document.documentElement.classList.contains("reduce-motion") || window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "instant" : "smooth" })}><Icon name="chevron" style={{ transform: "rotate(180deg)" }} /></button><button className="icon-button" aria-label={`Scroll ${title} right`} onClick={() => ref.current?.scrollBy({ left: ref.current.clientWidth * .8, behavior: document.documentElement.classList.contains("reduce-motion") || window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "instant" : "smooth" })}><Icon name="chevron" /></button></div>
    </div>
    <div className="shelf-track" ref={ref} tabIndex={0} aria-label={`${title} titles`}>
      <div style={{ width: virtualizer.getTotalSize(), height: compact ? 190 : 223, position: "relative" }}>
        {virtualizer.getVirtualItems().map((item) => {
          const channel = channels[item.index];
          const favorite = favorites.has(channel.id);
          return <article className={`media-card ${ranked ? "ranked-card" : ""}`} key={channel.id} style={{ position: "absolute", top: 0, left: 0, width: width - 12, transform: `translateX(${item.start}px)` }}>
            <button className="card-art-button" aria-label={`Play ${channel.name}`} onClick={() => onPlay(channel)}>
              <ChannelArtwork key={channel.id} channel={channel} />
              <span className="card-play"><Icon name="play" size={24} /></span>
              <span className={`card-badge ${channel.last_error ? "unavailable" : ""}`}>{channel.last_error ? "Unavailable" : channel.last_ok ? "Last checked OK" : "Stream"}</span>
              {ranked && <span className="card-rank">{item.index + 1}</span>}
            </button>
            <div className="card-bottom"><div><h3>{channel.name}</h3><p>{channel.group_title || "Your library"}</p></div><button className={`icon-button circle ${favorite ? "is-saved" : ""}`} aria-label={`${favorite ? "Remove" : "Add"} ${channel.name} ${favorite ? "from" : "to"} My List`} aria-pressed={favorite} onClick={() => onFavorite(channel.id)}><Icon name={favorite ? "check" : "plus"} size={17} /></button><button className="icon-button circle" aria-label={`Details for ${channel.name}`} onClick={() => onDetails(channel)}><Icon name="info" size={17} /></button></div>
          </article>;
        })}
      </div>
    </div>
  </section>;
}

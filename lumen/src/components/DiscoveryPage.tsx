import { useMemo } from "react";
import { Icon } from "./Icon";
import { t } from "../i18n/index.ts";
import { BrowseShelf, ContinueShelf, MediaShelf, UpcomingShelf } from "./MediaShelf";
import type { UpcomingCard } from "./MediaShelf";
import type { Board } from "../lib/board";
import { contentKind } from "../lib/discovery";
import type { BrowseTitle, Channel } from "../lib/types";
import type { CatalogShelf } from "../lib/discovery";

/** How many titles of each kind a cold-start explore shelf shows. */
const EXPLORE_SHELF_SIZE = 20;

interface DiscoveryPageProps {
  query: string;
  catalog: CatalogShelf[];
  recommended: Channel[];
  board: Board;
  upcoming: UpcomingCard[];
  browse: BrowseTitle[];
  personalized: boolean;
  libraryMatches: Channel[];
  favorites: Set<number>;
  compact: boolean;
  onPlayChannel: (channel: Channel) => void;
  onDetails: (channel: Channel) => void;
  onFavorite: (id: number) => void;
  onPlayFile: (path: string, name: string) => void;
  onOpenSources: () => void;
}

/**
 * Discovery is the TMDB catalogue, arranged into shelves by real TMDB metadata. Until titles
 * are matched, it falls back to recommendations from your sources. Files on this computer
 * live on the Local Device page.
 */
export function DiscoveryPage({
  query,
  catalog,
  recommended,
  board,
  upcoming,
  browse,
  personalized,
  libraryMatches,
  favorites,
  compact,
  onPlayChannel,
  onDetails,
  onFavorite,
  onPlayFile,
  onOpenSources,
}: DiscoveryPageProps) {
  const shelfProps = { favorites, onPlay: onPlayChannel, onDetails, onFavorite, compact };
  const showFallback = !query && catalog.length === 0 && board.recommended.length > 0;

  // A first run with no TMDB key and no watch history has only one shelf to show (the recommended
  // one above) even though the library can hold thousands of titles — the rest of the page was
  // dead space. These three draw straight from the library, split by kind, so there is something
  // to browse immediately. `recommended` already carries every channel ranked by the same scoring
  // as the shelf above; this only excludes what that shelf already shows, so nothing repeats.
  const exploreShelves = useMemo(() => {
    if (!showFallback) return null;
    const alreadyShown = new Set(board.recommended.map((channel) => channel.id));
    const byKind: Record<"movie" | "series" | "live", Channel[]> = { movie: [], series: [], live: [] };
    for (const channel of recommended) {
      if (alreadyShown.has(channel.id)) continue;
      const bucket = byKind[contentKind(channel)];
      if (bucket.length < EXPLORE_SHELF_SIZE) bucket.push(channel);
    }
    return byKind;
  }, [showFallback, board.recommended, recommended]);

  return <div className="discovery-page">
    <div className="discovery-body">
      {query && libraryMatches.length > 0 && <MediaShelf
        title={t("discovery.libraryMatches.title", { query })}
        subtitle={t("discovery.libraryMatches.subtitle")}
        channels={libraryMatches}
        {...shelfProps}
      />}

      {!query && <>
        <ContinueShelf items={board.continueWatching} compact={compact} onPlayChannel={onPlayChannel} onPlayFile={onPlayFile} />
        <MediaShelf title={t("discovery.nextUp.title")} subtitle={t("discovery.nextUp.subtitle")} channels={board.nextUp.map((item) => item.channel)} {...shelfProps} />
        <UpcomingShelf items={upcoming} compact={compact} onDetails={onDetails} />
        <MediaShelf title={t("discovery.fresh.title")} subtitle={t("discovery.fresh.subtitle")} channels={board.fresh} {...shelfProps} />
        <MediaShelf title={t("app.myList")} subtitle={t("discovery.myList.subtitle")} channels={board.myList} {...shelfProps} />
      </>}

      {!query && <BrowseShelf
        title={t("discovery.popular.title")}
        subtitle={t("discovery.popular.subtitle")}
        titles={browse}
        compact={compact}
      />}

      {!query && catalog.map((shelf) => <MediaShelf
        key={shelf.id}
        title={shelf.title}
        subtitle={shelf.subtitle}
        channels={shelf.channels}
        ranked={shelf.id === "top-rated"}
        {...shelfProps}
      />)}

      {showFallback && <>
        <MediaShelf
          title={personalized ? t("discovery.personalized.title") : t("discovery.fromSources.title")}
          subtitle={personalized ? t("discovery.personalized.subtitle") : t("discovery.fromSources.subtitle")}
          channels={board.recommended}
          {...shelfProps}
        />
        <p className="muted-note">{t("discovery.tmdbNote")}</p>
        {exploreShelves && exploreShelves.movie.length > 0 && <MediaShelf title={t("discovery.exploreMovies.title")} subtitle={t("discovery.exploreMovies.subtitle")} channels={exploreShelves.movie} {...shelfProps} />}
        {exploreShelves && exploreShelves.series.length > 0 && <MediaShelf title={t("discovery.exploreSeries.title")} subtitle={t("discovery.exploreSeries.subtitle")} channels={exploreShelves.series} {...shelfProps} />}
        {exploreShelves && exploreShelves.live.length > 0 && <MediaShelf title={t("discovery.exploreLive.title")} subtitle={t("discovery.exploreLive.subtitle")} channels={exploreShelves.live} {...shelfProps} />}
      </>}

      {query && libraryMatches.length === 0 && <div className="empty-state">
        <div className="empty-symbol"><Icon name="search" size={32} /></div>
        <h2>{t("discovery.noMatches.title", { query })}</h2>
        <p>{t("discovery.noMatches.body")}</p>
      </div>}

      {!query && catalog.length === 0 && recommended.length === 0 && browse.length === 0 && board.continueWatching.length === 0 && board.myList.length === 0 && <div className="empty-state">
        <div className="empty-symbol"><Icon name="compass" size={32} /></div>
        <h2>{t("discovery.empty.title")}</h2>
        <p>{t("discovery.empty.body")}</p>
        <div className="hero-buttons"><button className="primary-button" onClick={onOpenSources}><Icon name="plus" size={20} />{t("discovery.empty.addSource")}</button></div>
      </div>}
    </div>
  </div>;
}

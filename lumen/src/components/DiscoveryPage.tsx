import { Icon } from "./Icon";
import { BrowseShelf, ContinueShelf, MediaShelf, UpcomingShelf } from "./MediaShelf";
import type { UpcomingCard } from "./MediaShelf";
import type { Board } from "../lib/board";
import type { BrowseTitle, Channel } from "../lib/types";
import type { CatalogShelf } from "../lib/discovery";

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

  return <div className="discovery-page">
    <div className="discovery-body">
      {query && libraryMatches.length > 0 && <MediaShelf
        title={`In your library · “${query}”`}
        subtitle="Titles from your own sources that match this search."
        channels={libraryMatches}
        {...shelfProps}
      />}

      {!query && <>
        <ContinueShelf items={board.continueWatching} compact={compact} onPlayChannel={onPlayChannel} onPlayFile={onPlayFile} />
        <MediaShelf title="Next up" subtitle="The next episode of what you have been watching." channels={board.nextUp.map((item) => item.channel)} {...shelfProps} />
        <UpcomingShelf items={upcoming} compact={compact} onDetails={onDetails} />
        <MediaShelf title="New since your last visit" subtitle="Added to your sources since you last opened Lumen." channels={board.fresh} {...shelfProps} />
        <MediaShelf title="My List" subtitle="Titles you saved." channels={board.myList} {...shelfProps} />
      </>}

      {!query && <BrowseShelf
        title="Popular on TMDB"
        subtitle="Trending and top-rated titles from TMDB. They are not in your library, so they show details only."
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
          title={personalized ? "Because it’s your kind of thing" : "From your sources"}
          subtitle={personalized ? "Picked from your recently watched categories and saved titles." : "A selection from your own playlists."}
          channels={board.recommended}
          {...shelfProps}
        />
        <p className="muted-note">Genre shelves and posters appear once your titles are matched on TMDB. Add a TMDB key in Settings → Metadata.</p>
      </>}

      {query && libraryMatches.length === 0 && <div className="empty-state">
        <div className="empty-symbol"><Icon name="search" size={32} /></div>
        <h2>No matches for “{query}”.</h2>
        <p>Nothing in your sources matches that. Try a shorter word, or clear the search.</p>
      </div>}

      {!query && catalog.length === 0 && recommended.length === 0 && browse.length === 0 && board.continueWatching.length === 0 && board.myList.length === 0 && <div className="empty-state">
        <div className="empty-symbol"><Icon name="compass" size={32} /></div>
        <h2>Nothing to discover yet.</h2>
        <p>Add a playlist or catalogue source and its titles will show up here. Files on this computer are on the Local Device page.</p>
        <div className="hero-buttons"><button className="primary-button" onClick={onOpenSources}><Icon name="plus" size={20} />Add a source</button></div>
      </div>}
    </div>
  </div>;
}

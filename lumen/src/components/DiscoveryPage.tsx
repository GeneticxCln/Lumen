import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Icon } from "./Icon";
import type { IconName } from "./Icon";
import { MediaShelf } from "./MediaShelf";
import { errorMessage, importPlaylistFile, isTauriRuntime, listLocalDir, localThumbnail } from "../lib/tauri";
import { uniqueHistory } from "../lib/discovery";
import { MAX_ROOTS, MAX_SUBDIRS, MEDIA_ROOT_HINT, extensionOf, fileLabel, fileNameOf, formatClock, formatSize, isLocalPath, kindOf, mapLimit, parentOf } from "../lib/local";
import type { Channel, HistoryEntry, LocalFile } from "../lib/types";

/** One playable local entry, whether it came from a folder listing or from history. */
interface LocalItem {
  name: string;
  path: string;
  size?: number;
  position?: number;
  duration?: number;
  playedAt?: number;
}

interface LocalRow {
  title: string;
  subtitle?: string;
  items: LocalItem[];
}

interface DiscoveryPageProps {
  query: string;
  history: HistoryEntry[];
  historyError: string | null;
  recommended: Channel[];
  personalized: boolean;
  libraryMatches: Channel[];
  favorites: Set<number>;
  compact: boolean;
  busy: boolean;
  onPlayFile: (path: string, name?: string) => void | Promise<void>;
  onPlayChannel: (channel: Channel) => void;
  onDetails: (channel: Channel) => void;
  onFavorite: (id: number) => void;
  onLibraryChanged: () => void;
  onOpenFiles: () => void;
  onOpenSources: () => void;
}

const KIND_ICON: Record<string, IconName> = { video: "film", audio: "volume", playlist: "grid", other: "film" };
const KIND_LABEL: Record<string, string> = { video: "Video", audio: "Audio", playlist: "Playlist", other: "Media" };

function toItem(file: LocalFile): LocalItem {
  return { name: file.name, path: file.path, size: file.size };
}

function matches(item: LocalItem, query: string): boolean {
  if (!query) return true;
  return `${item.name} ${item.path}`.toLocaleLowerCase().includes(query.toLocaleLowerCase());
}

function usePosterData(path: string, active: boolean) {
  const [poster, setPoster] = useState<string | null>(null);
  useEffect(() => {
    if (!active) return;
    let current = true;
    localThumbnail(path).then((url) => { if (current) setPoster(url); }).catch(() => {
      // Fall back to the branded title tile when ffmpeg is absent or this video has no frame.
    });
    return () => { current = false; };
  }, [path, active]);
  return poster;
}

function LocalCard({ item, folder, onPlay }: { item: LocalItem; folder: string; onPlay: (item: LocalItem) => void }) {
  const kind = kindOf(item.name);
  const artRef = useRef<HTMLButtonElement>(null);
  const [nearViewport, setNearViewport] = useState(false);
  useEffect(() => {
    if (kind !== "video" || !artRef.current) return;
    const element = artRef.current;
    if (!("IntersectionObserver" in window)) { setNearViewport(true); return; }
    const observer = new IntersectionObserver((observations) => {
      if (observations.some((observation) => observation.isIntersecting)) {
        setNearViewport(true);
        observer.disconnect();
      }
    }, { rootMargin: "180px" });
    observer.observe(element);
    return () => observer.disconnect();
  }, [item.path, kind]);
  const poster = usePosterData(item.path, kind === "video" && nearViewport);
  const percent = item.duration && item.position ? Math.min(100, Math.round((item.position / item.duration) * 100)) : 0;
  const meta = [formatSize(item.size), percent > 0 ? `${formatClock(item.position!)} of ${formatClock(item.duration!)}` : folder].filter(Boolean).join(" · ");
  return <article className="local-card">
    <button ref={artRef} className={`local-card-art ${poster ? "has-poster" : ""}`} aria-label={`Play ${fileLabel(item.name)}`} onClick={() => onPlay(item)}>
      {poster && <img className="local-card-poster" src={poster} alt="" loading="lazy" />}
      {!poster && <span className={`local-card-glyph kind-${kind}`}><Icon name={KIND_ICON[kind]} size={22} /></span>}
      <span className="local-card-ext">{extensionOf(item.name).toUpperCase() || KIND_LABEL[kind]}</span>
      <span className="card-play"><Icon name="play" size={24} /></span>
      {percent > 0 && <span className="local-card-progress"><i style={{ width: `${percent}%` }} /></span>}
    </button>
    <div className="local-card-body">
      <h3 title={item.path}>{fileLabel(item.name)}</h3>
      <p>{meta}</p>
    </div>
  </article>;
}

function HeroPoster({ path }: { path: string }) {
  const poster = usePosterData(path, true);
  return poster ? <div className="local-hero-poster-wrap" aria-hidden="true"><img className="local-hero-poster" src={poster} alt="" /></div> : null;
}

function LocalRowView({ row, onPlay }: { row: LocalRow; onPlay: (item: LocalItem) => void }) {
  const track = useRef<HTMLDivElement>(null);
  const scroll = (direction: number) => track.current?.scrollBy({ left: direction * track.current.clientWidth * .8, behavior: document.documentElement.classList.contains("reduce-motion") ? "instant" : "smooth" });
  if (!row.items.length) return null;
  return <section className="media-shelf local-row" aria-label={row.title}>
    <div className="shelf-heading"><div><h2>{row.title}<span className="shelf-count">{row.items.length}</span></h2>{row.subtitle && <p>{row.subtitle}</p>}</div>
      <div className="shelf-arrows"><button className="icon-button" aria-label={`Scroll ${row.title} left`} onClick={() => scroll(-1)}><Icon name="chevron" style={{ transform: "rotate(180deg)" }} /></button><button className="icon-button" aria-label={`Scroll ${row.title} right`} onClick={() => scroll(1)}><Icon name="chevron" /></button></div>
    </div>
    <div className="local-track" ref={track} tabIndex={0} aria-label={`${row.title} files`}>
      {row.items.map((item) => <LocalCard key={item.path} item={item} folder={parentOf(item.path).split(/[\\/]/).pop() || ""} onPlay={onPlay} />)}
    </div>
  </section>;
}

/**
 * Discovery is about this device: folders, video files and resume points that are already on
 * the computer, read straight from the OS through the local-file bridge. Nothing here is
 * inferred or fetched — if a file is not on the disk it is not on this page.
 */
export function DiscoveryPage({
  query,
  history,
  historyError,
  recommended,
  personalized,
  libraryMatches,
  favorites,
  compact,
  busy,
  onPlayFile,
  onPlayChannel,
  onDetails,
  onFavorite,
  onLibraryChanged,
  onOpenFiles,
  onOpenSources,
}: DiscoveryPageProps) {
  const [rows, setRows] = useState<LocalRow[]>([]);
  const [scanning, setScanning] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const [browsing, setBrowsing] = useState(false);
  const [browsePath, setBrowsePath] = useState("");
  const [entries, setEntries] = useState<LocalFile[]>([]);
  const [browseLoading, setBrowseLoading] = useState(false);
  const [browseError, setBrowseError] = useState<string | null>(null);

  const localHistory = useMemo(() => uniqueHistory(history).filter((entry) => isLocalPath(entry.path)), [history]);
  const continueWatching = useMemo(() => localHistory
    .filter((entry) => entry.position > 10 && entry.duration > 0)
    .map((entry) => ({ name: fileNameOf(entry.path), path: entry.path, position: entry.position, duration: entry.duration, playedAt: entry.played_at })), [localHistory]);
  const recentlyPlayed = useMemo(() => localHistory
    .slice(0, 12)
    .map((entry) => ({ name: fileNameOf(entry.path), path: entry.path, position: entry.position, duration: entry.duration, playedAt: entry.played_at })), [localHistory]);

  const scan = useCallback(async () => {
    setScanning(true);
    setError(null);
    try {
      const home = await listLocalDir("");
      const dirs = home.filter((entry) => entry.is_dir);
      const loose = home.filter((entry) => !entry.is_dir);
      const roots = [
        ...dirs.filter((dir) => MEDIA_ROOT_HINT.test(dir.name)),
        ...dirs.filter((dir) => !MEDIA_ROOT_HINT.test(dir.name)),
      ].slice(0, MAX_ROOTS);
      const listings = await mapLimit(roots, 3, async (dir) => {
        try { return await listLocalDir(dir.path); } catch { return [] as LocalFile[]; }
      });

      const next: LocalRow[] = [];
      if (loose.length) next.push({ title: "Loose files in your folder", subtitle: "Media sitting directly beside your folders.", items: loose.map(toItem) });

      const deeper: { label: string; path: string }[] = [];
      roots.forEach((dir, index) => {
        const listing = listings[index] || [];
        const files = listing.filter((entry) => !entry.is_dir);
        if (files.length) next.push({ title: `In ${dir.name}`, items: files.map(toItem) });
        if (deeper.length < MAX_SUBDIRS) {
          listing.filter((entry) => entry.is_dir).slice(0, 2).forEach((sub) => {
            if (deeper.length < MAX_SUBDIRS) deeper.push({ label: `${dir.name}/${sub.name}`, path: sub.path });
          });
        }
      });

      const deepListings = await mapLimit(deeper, 3, async (folder) => {
        try { return await listLocalDir(folder.path); } catch { return [] as LocalFile[]; }
      });
      deepListings.forEach((listing, index) => {
        const files = listing.filter((entry) => !entry.is_dir);
        if (files.length) next.push({ title: `In ${deeper[index].label}`, items: files.map(toItem) });
      });

      setRows(next);
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setScanning(false);
    }
  }, []);

  useEffect(() => { void scan(); }, [scan, revision]);

  const browse = useCallback(async (path: string) => {
    setBrowseLoading(true);
    setBrowseError(null);
    try {
      const listing = await listLocalDir(path);
      setEntries([...listing].sort((a, b) => Number(b.is_dir) - Number(a.is_dir) || a.name.localeCompare(b.name)));
      setBrowsePath(path);
    } catch (failure) {
      setBrowseError(errorMessage(failure));
    } finally {
      setBrowseLoading(false);
    }
  }, []);

  const openBrowser = useCallback((path = "") => {
    setBrowsing(true);
    void browse(path);
  }, [browse]);

  const playLocal = useCallback(async (item: LocalItem) => {
    setStatus(null);
    try {
      await onPlayFile(item.path, item.name);
    } catch {
      // App surfaces playback failures in its own notice bar.
    }
  }, [onPlayFile]);

  const openEntry = useCallback(async (entry: LocalFile) => {
    if (entry.is_dir) { await browse(entry.path); return; }
    if (kindOf(entry.name) === "playlist") {
      setStatus(null);
      try {
        await importPlaylistFile(entry.path);
        setStatus(`Imported “${entry.name}” as a source.`);
        onLibraryChanged();
        setRevision((value) => value + 1);
      } catch (failure) {
        setBrowseError(errorMessage(failure));
      }
      return;
    }
    await playLocal({ name: entry.name, path: entry.path, size: entry.size });
  }, [browse, onLibraryChanged, playLocal]);

  const filteredRows = useMemo(() => rows
    .map((row) => ({ ...row, items: row.items.filter((item) => matches(item, query)) }))
    .filter((row) => row.items.length), [rows, query]);
  const continueItems = useMemo(() => continueWatching.filter((item) => matches(item, query)), [continueWatching, query]);
  const recentItems = useMemo(() => recentlyPlayed.filter((item) => matches(item, query)), [recentlyPlayed, query]);
  const visibleEntries = useMemo(() => entries.filter((entry) => matches({ name: entry.name, path: entry.path }, query)), [entries, query]);

  const feature = continueItems[0] || recentItems[0] || filteredRows[0]?.items[0] || null;
  const totalLocal = rows.reduce((count, row) => count + row.items.length, 0);
  const hasLocalContent = totalLocal > 0;
  const nothingToShow = !scanning && !hasLocalContent && localHistory.length === 0;

  const crumbs = useMemo(() => {
    const parts = browsePath.split(/[\\/]/).filter(Boolean);
    return parts.slice(Math.max(0, parts.length - 3));
  }, [browsePath]);

  return <div className="discovery-page">
    <section className="local-hero">
      <div className="local-hero-glow" />
      {feature && kindOf(feature.name) === "video" && !isTauriRuntime() && <HeroPoster path={feature.path} />}
      {!feature || kindOf(feature.name) !== "video" ? <div className="local-hero-mark"><Icon name="monitor" size={96} /></div> : null}
      <div className="local-hero-content">
        <div className="hero-kicker"><span className="mini-brand">L</span> ON THIS DEVICE</div>
        <p className="eyebrow">{feature ? "FROM YOUR OWN FOLDERS" : "YOUR FILES, ON PLAY"}</p>
        <h1>{feature ? fileLabel(feature.name) : <>Your own files,<br />ready to play.</>}</h1>
        <div className="hero-meta">
          {feature
            ? <><span className="match-label">{feature.playedAt ? "In progress" : "On this device"}</span><span>{parentOf(feature.path).split(/[\\/]/).pop() || "Local media"}</span>{feature.duration ? <span className="quality-tag">{formatClock(feature.duration)}</span> : null}</>
            : <><span className="match-label">Local first</span><span>Folders · Video · Audio</span><span className="quality-tag">MPV</span></>}
        </div>
        {feature && feature.duration ? <div className="local-hero-progress" aria-hidden="true"><i style={{ width: `${Math.min(100, Math.round(((feature.position || 0) / feature.duration) * 100))}%` }} /></div> : null}
        <p className="hero-description">
          {feature
            ? `Played with the native engine straight from ${parentOf(feature.path)}. Resume position is remembered per file, so this always picks up where you stopped.`
            : "Lumen reads the media already on this computer — no uploads, no accounts, no catalogues. Point it at a folder and it shows up right here."}
        </p>
        <div className="hero-buttons">
          {feature
            ? <button className="primary-button" disabled={busy} onClick={() => void playLocal(feature)}><Icon name="play" size={23} />{feature.position && feature.position > 10 ? `Resume · ${formatClock(feature.position)}` : "Play"}</button>
            : <button className="primary-button" onClick={() => openBrowser("")}><Icon name="folder" size={23} />Browse folders</button>}
          {feature && <button className="secondary-button" onClick={() => openBrowser("")}><Icon name="folder" size={23} />Browse folders</button>}
          <button className="secondary-button" onClick={onOpenFiles}><Icon name="settings" size={23} />Local file settings</button>
        </div>
        <div className="hero-footnote"><span className="tiny-dot" />{scanning ? "Reading your folders…" : hasLocalContent ? `${totalLocal} local ${totalLocal === 1 ? "file" : "files"} found · Nothing leaves this device` : "Nothing found in your usual folders yet"}</div>
      </div>
      <div className="hero-side-label">{scanning ? "SCANNING THIS DEVICE" : hasLocalContent ? "ON THIS DEVICE" : "WAITING FOR FILES"}</div>
    </section>

    <div className="discovery-body">
      {status && <div className="app-notice discovery-notice" role="status"><span>{status}</span><button className="icon-button" aria-label="Dismiss message" onClick={() => setStatus(null)}><Icon name="close" /></button></div>}
      {error && <div className="app-notice discovery-notice" role="alert"><div><strong>Couldn’t read your local folders</strong><p>{error}</p></div><button onClick={() => setRevision((value) => value + 1)}>Retry</button></div>}

      {scanning && <div className="skeleton-row" role="status" aria-label="Scanning this device">{[1, 2, 3, 4].map((id) => <div key={id} />)}</div>}

      {query && libraryMatches.length > 0 && <MediaShelf
        title={`In your library · “${query}”`}
        subtitle="Titles from your own sources that match this search."
        channels={libraryMatches}
        favorites={favorites}
        onPlay={onPlayChannel}
        onDetails={onDetails}
        onFavorite={onFavorite}
        compact={compact}
      />}

      {!scanning && continueItems.length > 0 && <LocalRowView row={{ title: "Continue watching", subtitle: "Pick up exactly where you stopped.", items: continueItems }} onPlay={(item) => void playLocal(item)} />}
      {!scanning && recentItems.length > 0 && <LocalRowView row={{ title: "Recently played on this device", subtitle: "Your last local sessions.", items: recentItems }} onPlay={(item) => void playLocal(item)} />}
      {!scanning && filteredRows.map((row) => <LocalRowView key={row.title} row={row} onPlay={(item) => void playLocal(item)} />)}

      {!scanning && !query && recommended.length > 0 && <MediaShelf
        title={personalized ? "Because it’s your kind of thing" : "From your sources"}
        subtitle={personalized ? "Picked from your recently watched categories and saved titles." : "A selection from your own playlists."}
        channels={recommended.slice(0, 20)}
        favorites={favorites}
        onPlay={onPlayChannel}
        onDetails={onDetails}
        onFavorite={onFavorite}
        compact={compact}
      />}

      {nothingToShow && <div className="empty-state">
        <div className="empty-symbol"><Icon name="folder" size={32} /></div>
        <h2>No local media found yet.</h2>
        <p>Looked in your usual folders for video, audio and playlist files. Point Lumen at another folder, or add a playlist source — both show up here.</p>
        <div className="hero-buttons"><button className="primary-button" onClick={() => openBrowser("")}><Icon name="folder" size={20} />Browse folders</button><button className="secondary-button" onClick={onOpenSources}><Icon name="plus" size={20} />Add a source</button></div>
      </div>}

      {query && !libraryMatches.length && !continueItems.length && !recentItems.length && !filteredRows.length && <div className="empty-state">
        <div className="empty-symbol"><Icon name="search" size={32} /></div>
        <h2>No matches for “{query}”.</h2>
        <p>Nothing on this device or in your sources matches that. Try a shorter word, or clear the search.</p>
      </div>}

      <section className="folder-browser">
        <div className="folder-browser-head">
          <div className="folder-browser-title"><span className="settings-title-icon"><Icon name="folder" size={20} /></span><div><h2>Browse this computer</h2><p>Folders, video, audio and playlist files.</p></div></div>
          <button className="secondary-button" aria-expanded={browsing} onClick={() => (browsing ? setBrowsing(false) : openBrowser(browsePath || ""))}>{browsing ? "Hide" : "Open browser"}</button>
        </div>

        {browsing && <>
          <div className="local-path-bar">
            <button type="button" onClick={() => void browse("")} title="Your home folder"><Icon name="folder" size={15} />Home</button>
            <button type="button" onClick={() => void browse(parentOf(browsePath))} disabled={!parentOf(browsePath)} title="Parent folder"><Icon name="arrow" size={15} />Up</button>
            <div className="local-crumbs">{crumbs.length === 0 ? <span className="muted-note">Home</span> : crumbs.map((crumb, index) => <span key={`${crumb}-${index}`}>{index > 0 && <Icon name="chevron" size={12} />}<span className="crumb">{crumb}</span></span>)}</div>
            <button type="button" className="icon-button" aria-label="Rescan this device" onClick={() => setRevision((value) => value + 1)}><Icon name="refresh" size={17} /></button>
          </div>
          {browseError && <div className="dialog-error" role="alert">{browseError}</div>}
          <div className="local-entries-list">
            {browseLoading
              ? <div className="loading">Reading folder…</div>
              : visibleEntries.length === 0
                ? <div className="empty-channels">Nothing to play in this folder.</div>
                : visibleEntries.map((entry) => <button key={entry.path} type="button" className="local-entry-item" onClick={() => void openEntry(entry)}>
                  <span className={`local-entry-icon kind-${kindOf(entry.name)}`}><Icon name={entry.is_dir ? "folder" : KIND_ICON[kindOf(entry.name)]} size={17} /></span>
                  <span className="local-entry-name" title={entry.path}>{entry.is_dir ? entry.name : fileLabel(entry.name)}</span>
                  <span className="local-entry-size">{entry.is_dir ? "Folder" : [formatSize(entry.size), extensionOf(entry.name).toUpperCase()].filter(Boolean).join(" · ")}</span>
                </button>)}
          </div>
        </>}
      </section>

      {historyError && <p className="muted-note" role="status">Recently played is unavailable: {historyError}</p>}
    </div>
  </div>;
}

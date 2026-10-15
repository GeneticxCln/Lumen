import { useState, useEffect, useCallback } from "react";
import {
  listLocalDir,
  getHistory,
  importPlaylistFile,
  LocalFile,
  HistoryEntry,
  errorMessage,
} from "../lib/tauri";

interface LocalBrowserProps {
  onPlayFile: (path: string, name?: string) => void | Promise<void>;
  onPlaylistImported?: () => void;
}

function formatFileSize(bytes?: number): string {
  if (bytes === undefined || bytes === null) return "";
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  const gb = mb / 1024;
  return `${gb.toFixed(2)} GB`;
}

function formatDuration(sec: number): string {
  const m = Math.floor(sec / 60);
  const s = Math.floor(sec % 60);
  return `${m}:${s.toString().padStart(2, "0")}`;
}

export function LocalBrowser({ onPlayFile, onPlaylistImported }: LocalBrowserProps) {
  const [currentPath, setCurrentPath] = useState<string>("");
  const [entries, setEntries] = useState<LocalFile[]>([]);
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [loading, setLoading] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);

  const browse = useCallback(async (path: string) => {
    setLoading(true);
    setError(null);
    try {
      const items = await listLocalDir(path);
      setEntries(items);
      if (path) {
        setCurrentPath(path);
      } else if (items.length > 0) {
        const firstPath = items[0].path;
        const parent = firstPath.substring(0, firstPath.lastIndexOf("/")) || "/";
        setCurrentPath(parent);
      }
    } catch (failure: unknown) {
      setError(errorMessage(failure) || "Failed to read directory");
    } finally {
      setLoading(false);
    }
  }, []);

  const loadHistory = useCallback(async () => {
    try {
      const h = await getHistory(10);
      setHistory(h);
    } catch (failure) {
      setError(`Could not load history: ${errorMessage(failure)}`);
    }
  }, []);

  useEffect(() => {
    browse("");
    loadHistory();
  }, [browse, loadHistory]);

  const navigateUp = () => {
    if (!currentPath || currentPath === "/") return;
    const parent = currentPath.substring(0, currentPath.lastIndexOf("/")) || "/";
    browse(parent);
  };

  const handleEntryClick = async (entry: LocalFile) => {
    if (entry.is_dir) {
      browse(entry.path);
      return;
    }
    const lower = entry.name.toLowerCase();
    if (lower.endsWith(".m3u") || lower.endsWith(".m3u8")) {
      try {
        await importPlaylistFile(entry.path);
        onPlaylistImported?.();
      } catch (failure: unknown) {
        setError(errorMessage(failure) || "Failed to import playlist");
      }
    } else {
      try {
        await onPlayFile(entry.path, entry.name);
        await loadHistory();
      } catch (failure) {
        setError(errorMessage(failure));
      }
    }
  };

  return (
    <div className="local-browser">
      <div className="local-path-bar">
        <button type="button" onClick={() => browse("")} title="Home directory">
          Home
        </button>
        <button
          type="button"
          onClick={navigateUp}
          disabled={!currentPath || currentPath === "/"}
          title="Parent directory"
        >
          ↑ Up
        </button>
        <input
          value={currentPath}
          onChange={(e) => setCurrentPath(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") browse(currentPath);
          }}
          aria-label="Local folder path"
          placeholder="/home/user/Videos"
        />
        <button type="button" onClick={() => browse(currentPath)}>
          Go
        </button>
      </div>

      {error && <div className="dialog-error">{error}</div>}

      {loading ? (
        <div className="loading">Reading folder...</div>
      ) : (
        <div className="local-entries-list">
          {entries.length === 0 ? (
            <div className="empty-channels">No media files or folders in this directory.</div>
          ) : (
            entries.map((entry) => (
              <button
                type="button"
                key={entry.path}
                className="local-entry-item"
                onClick={() => handleEntryClick(entry)}
              >
                <span className="local-entry-icon">{entry.is_dir ? "[DIR]" : "▶"}</span>
                <span className="local-entry-name">{entry.name}</span>
                {!entry.is_dir && entry.size !== undefined && (
                  <span className="local-entry-size">{formatFileSize(entry.size)}</span>
                )}
              </button>
            ))
          )}
        </div>
      )}

      {history.length > 0 && (
        <div className="recent-history">
          <h4>Recently Played (Resume Supported)</h4>
          <div className="history-items">
            {history.slice(0, 5).map((item) => {
              const label = item.path.split("/").pop() || item.path;
              return (
                <button
                  type="button"
                  key={item.id}
                  className="local-entry-item"
                  onClick={() => Promise.resolve(onPlayFile(item.path, label)).catch((failure) => setError(errorMessage(failure)))}
                >
                  <span className="local-entry-icon">↺</span>
                  <span className="local-entry-name" title={item.path}>
                    {label}
                  </span>
                  {item.position > 0 && item.duration > 0 && (
                    <span className="local-entry-size">
                      {formatDuration(item.position)} / {formatDuration(item.duration)}
                    </span>
                  )}
                </button>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}

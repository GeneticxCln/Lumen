import { useState } from "react";
import {
  importPlaylistFile,
  importPlaylistUrl,
  extractUrl,
  harvestPage,
  errorText,
} from "../lib/tauri";
import { LocalBrowser } from "./LocalBrowser";
import { Modal } from "./Modal";

interface SourceDialogProps {
  isOpen: boolean;
  onClose: () => void;
  onSourceAdded: () => void;
  onPlayFile?: (path: string, name?: string, headers?: Record<string, string>, audioUrl?: string) => Promise<void>;
}

const PUBLIC_FTA_CATALOGS = [
  {
    name: "IPTV-org: News (Public FTA)",
    url: "https://iptv-org.github.io/iptv/categories/news.m3u",
    category: "Category",
  },
  {
    name: "IPTV-org: Science & Education (Public FTA)",
    url: "https://iptv-org.github.io/iptv/categories/science.m3u",
    category: "Category",
  },
  {
    name: "IPTV-org: Documentary (Public FTA)",
    url: "https://iptv-org.github.io/iptv/categories/documentary.m3u",
    category: "Category",
  },
  {
    name: "IPTV-org: Music (Public FTA)",
    url: "https://iptv-org.github.io/iptv/categories/music.m3u",
    category: "Category",
  },
  {
    name: "IPTV-org: United Kingdom (Public FTA)",
    url: "https://iptv-org.github.io/iptv/countries/uk.m3u",
    category: "Country",
  },
  {
    name: "IPTV-org: United States (Public FTA)",
    url: "https://iptv-org.github.io/iptv/countries/us.m3u",
    category: "Country",
  },
  {
    name: "IPTV-org: France (Public FTA)",
    url: "https://iptv-org.github.io/iptv/countries/fr.m3u",
    category: "Country",
  },
  {
    name: "IPTV-org: Germany (Public FTA)",
    url: "https://iptv-org.github.io/iptv/countries/de.m3u",
    category: "Country",
  },
];

export function SourceDialog({
  isOpen,
  onClose,
  onSourceAdded,
  onPlayFile,
}: SourceDialogProps) {
  const [tab, setTab] = useState<"file" | "url" | "extract" | "catalog" | "local">("url");
  const [url, setUrl] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [statusMessage, setStatusMessage] = useState<string | null>(null);

  if (!isOpen) return null;

  // `errorText` unwraps the serialized PlayerError Tauri rejects with; the fallback covers
  // shapes that carry no readable message.
  const formatErr = (e: unknown, fallback: string) => errorText(e) ?? fallback;

  const selectFile = async (media: boolean) => {
    setError(null);
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({ filters: [{ name: media ? "Media" : "M3U Playlist", extensions: media ? ["mp4", "mkv", "webm", "avi", "mov", "ts", "mp3", "flac", "wav"] : ["m3u", "m3u8"] }] });
      if (typeof selected !== "string") return;
      if (media) { await onPlayFile?.(selected); onClose(); }
      else await handleFileImport(selected);
    } catch (failure) { setError(formatErr(failure, "Unable to open file picker")); }
  };

  const handleFileImport = async (filePath: string) => {
    setLoading(true);
    setError(null);
    try {
      await importPlaylistFile(filePath);
      onSourceAdded();
      onClose();
    } catch (e: unknown) {
      setError(formatErr(e, "Failed to import file"));
    } finally {
      setLoading(false);
    }
  };

  const handleUrlImport = async (targetUrl?: string, customName?: string) => {
    const finalUrl = (targetUrl ?? url).trim();
    if (!finalUrl) return;
    try {
      const parsed = new URL(finalUrl);
      if (!["http:", "https:"].includes(parsed.protocol)) throw new Error("Unsupported protocol");
    } catch { setError("Enter a valid http:// or https:// URL."); return; }
    setLoading(true);
    setError(null);
    try {
      await importPlaylistUrl(finalUrl, customName);
      onSourceAdded();
      onClose();
    } catch (e: unknown) {
      setError(formatErr(e, "Failed to import URL"));
    } finally {
      setLoading(false);
    }
  };

  const handleExtract = async () => {
    if (!url.trim()) return;
    setLoading(true);
    setError(null);
    setStatusMessage(null);
    try {
      const res = await extractUrl(url.trim());
      onSourceAdded();
      await onPlayFile?.(res.url, res.title, res.headers, res.audio_url);
      onClose();
    } catch (e: unknown) {
      setError(formatErr(e, "Failed to extract stream"));
    } finally {
      setLoading(false);
    }
  };

  const handleHarvest = async () => {
    if (!url.trim()) return;
    setLoading(true);
    setError(null);
    setStatusMessage(null);
    try {
      const res = await harvestPage(url.trim());
      if (res.urls.length === 0) {
        setError("No .m3u or .m3u8 links found on that page.");
      } else {
        onSourceAdded();
        onClose();
      }
    } catch (e: unknown) {
      setError(formatErr(e, "Failed to harvest page"));
    } finally {
      setLoading(false);
    }
  };

  return (
    <Modal open={isOpen} onClose={onClose} title="Add a source">

        <div className="dialog-tabs">
          <button
            className={tab === "file" ? "active" : ""}
            onClick={() => setTab("file")}
          >
            M3U File
          </button>
          <button
            className={tab === "url" ? "active" : ""}
            onClick={() => setTab("url")}
          >
            M3U URL
          </button>
          <button
            className={tab === "extract" ? "active" : ""}
            onClick={() => setTab("extract")}
          >
            Extract Page
          </button>
          <button
            className={tab === "catalog" ? "active" : ""}
            onClick={() => setTab("catalog")}
          >
            FTA Catalogs
          </button>
          <button
            className={tab === "local" ? "active" : ""}
            onClick={() => setTab("local")}
          >
            Local Files
          </button>
        </div>

        {error && <div className="dialog-error" role="alert">{error}</div>}
        {statusMessage && <div className="dialog-status" role="status">{statusMessage}</div>}

        {tab === "file" && (
          <div className="dialog-content">
            <p>Select a local M3U / M3U8 playlist or video file</p>
            <div className="extract-buttons"><button disabled={loading} onClick={() => void selectFile(false)}>Select M3U / M3U8 file</button><button disabled={loading} onClick={() => void selectFile(true)}>Open video / audio</button></div>
          </div>
        )}

        {tab === "url" && (
          <div className="dialog-content">
            <p>Enter an M3U / M3U8 playlist URL</p>
            <input
              type="url"
              aria-label="Source URL"
              placeholder="https://example.com/playlist.m3u8"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
            />
            <button onClick={() => handleUrlImport()} disabled={loading || !url.trim()}>
              {loading ? "Importing..." : "Import Playlist"}
            </button>
          </div>
        )}

        {tab === "extract" && (
          <div className="dialog-content">
            <p>Paste a page URL to extract its direct stream (yt-dlp) or harvest M3U links</p>
            <input
              type="url"
              aria-label="Source URL"
              placeholder="https://youtube.com/watch?v=... or any page with embedded streams"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
            />
            <div className="extract-buttons">
              <button onClick={handleExtract} disabled={loading || !url.trim()}>
                {loading ? "Working..." : "Extract & Play"}
              </button>
              <button onClick={handleHarvest} disabled={loading || !url.trim()}>
                {loading ? "Working..." : "Harvest M3U Links"}
              </button>
            </div>
          </div>
        )}

        {tab === "catalog" && (
          <div className="dialog-content">
            <p>Import public free-to-air (FTA) channel slices from iptv-org</p>
            <div className="catalog-list">
              {PUBLIC_FTA_CATALOGS.map((cat) => (
                <div key={cat.url} className="catalog-item">
                  <div>
                    <div className="catalog-name">{cat.name}</div>
                    <div className="catalog-meta">{cat.category}</div>
                  </div>
                  <button
                    disabled={loading}
                    onClick={() => handleUrlImport(cat.url, cat.name)}
                  >
                    Import
                  </button>
                </div>
              ))}
            </div>
          </div>
        )}

        {tab === "local" && (
          <div className="dialog-content">
            <LocalBrowser
              onPlayFile={async (path, name) => {
                await onPlayFile?.(path, name);
                onClose();
              }}
              onPlaylistImported={() => {
                onSourceAdded();
                onClose();
              }}
            />
          </div>
        )}
    </Modal>
  );
}

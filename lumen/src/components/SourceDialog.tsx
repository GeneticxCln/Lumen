import { useState } from "react";
import {
  importPlaylistFile,
  importPlaylistUrl,
  extractUrl,
  harvestPage,
  errorText,
} from "../lib/tauri";
import { BulkLinks } from "./BulkLinks";
import { LocalBrowser } from "./LocalBrowser";
import { Modal } from "./Modal";
import { t } from "../i18n/index.ts";

interface SourceDialogProps {
  isOpen: boolean;
  onClose: () => void;
  onSourceAdded: () => void;
  onPlayFile?: (path: string, name?: string, headers?: Record<string, string>, audioUrl?: string, channelId?: number) => Promise<void>;
}

/** The public free-to-air slices. Names and kinds are shown to the user, so they come from the catalog. */
function publicFtaCatalogs() {
  return [
    { name: t("source.catalog.news"), url: "https://iptv-org.github.io/iptv/categories/news.m3u", category: t("source.catalog.category") },
    { name: t("source.catalog.science"), url: "https://iptv-org.github.io/iptv/categories/science.m3u", category: t("source.catalog.category") },
    { name: t("source.catalog.documentary"), url: "https://iptv-org.github.io/iptv/categories/documentary.m3u", category: t("source.catalog.category") },
    { name: t("source.catalog.music"), url: "https://iptv-org.github.io/iptv/categories/music.m3u", category: t("source.catalog.category") },
    { name: t("source.catalog.uk"), url: "https://iptv-org.github.io/iptv/countries/uk.m3u", category: t("source.catalog.country") },
    { name: t("source.catalog.us"), url: "https://iptv-org.github.io/iptv/countries/us.m3u", category: t("source.catalog.country") },
    { name: t("source.catalog.fr"), url: "https://iptv-org.github.io/iptv/countries/fr.m3u", category: t("source.catalog.country") },
    { name: t("source.catalog.de"), url: "https://iptv-org.github.io/iptv/countries/de.m3u", category: t("source.catalog.country") },
  ];
}

export function SourceDialog({
  isOpen,
  onClose,
  onSourceAdded,
  onPlayFile,
}: SourceDialogProps) {
  const [tab, setTab] = useState<"file" | "url" | "bulk" | "extract" | "catalog" | "local">("url");
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
      const selected = await open({ filters: [{ name: media ? t("source.filter.media") : t("source.filter.playlist"), extensions: media ? ["mp4", "mkv", "webm", "avi", "mov", "ts", "mp3", "flac", "wav"] : ["m3u", "m3u8"] }] });
      if (typeof selected !== "string") return;
      if (media) { await onPlayFile?.(selected); onClose(); }
      else await handleFileImport(selected);
    } catch (failure) { setError(formatErr(failure, t("source.error.picker"))); }
  };

  const handleFileImport = async (filePath: string) => {
    setLoading(true);
    setError(null);
    try {
      await importPlaylistFile(filePath);
      onSourceAdded();
      onClose();
    } catch (e: unknown) {
      setError(formatErr(e, t("source.error.importFile")));
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
    } catch { setError(t("source.error.invalidUrl")); return; }
    setLoading(true);
    setError(null);
    try {
      await importPlaylistUrl(finalUrl, customName);
      onSourceAdded();
      onClose();
    } catch (e: unknown) {
      setError(formatErr(e, t("source.error.importUrl")));
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
      // Play the saved entry by id: its stream link is resolved at play time (this result is reused).
      await onPlayFile?.(res.page_url, res.title, undefined, undefined, res.channel_id);
      onClose();
    } catch (e: unknown) {
      setError(formatErr(e, t("source.error.extract")));
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
        setError(t("source.error.noLinks"));
      } else {
        onSourceAdded();
        onClose();
      }
    } catch (e: unknown) {
      setError(formatErr(e, t("source.error.harvest")));
    } finally {
      setLoading(false);
    }
  };

  return (
    <Modal open={isOpen} onClose={onClose} title={t("source.title")}>

        <div className="dialog-tabs">
          <button
            className={tab === "file" ? "active" : ""}
            onClick={() => setTab("file")}
          >
            {t("source.tab.file")}
          </button>
          <button
            className={tab === "url" ? "active" : ""}
            onClick={() => setTab("url")}
          >
            {t("source.tab.url")}
          </button>
          <button
            className={tab === "bulk" ? "active" : ""}
            onClick={() => setTab("bulk")}
          >
            {t("bulk.tab")}
          </button>
          <button
            className={tab === "extract" ? "active" : ""}
            onClick={() => setTab("extract")}
          >
            {t("source.tab.extract")}
          </button>
          <button
            className={tab === "catalog" ? "active" : ""}
            onClick={() => setTab("catalog")}
          >
            {t("source.tab.catalog")}
          </button>
          <button
            className={tab === "local" ? "active" : ""}
            onClick={() => setTab("local")}
          >
            {t("source.tab.local")}
          </button>
        </div>

        {error && <div className="dialog-error" role="alert">{error}</div>}
        {statusMessage && <div className="dialog-status" role="status">{statusMessage}</div>}

        {tab === "file" && (
          <div className="dialog-content">
            <p>{t("source.file.intro")}</p>
            <div className="extract-buttons"><button disabled={loading} onClick={() => void selectFile(false)}>{t("source.file.selectPlaylist")}</button><button disabled={loading} onClick={() => void selectFile(true)}>{t("source.file.openMedia")}</button></div>
          </div>
        )}

        {tab === "url" && (
          <div className="dialog-content">
            <p>{t("source.url.intro")}</p>
            <input
              type="url"
              aria-label={t("source.url.label")}
              placeholder={t("source.url.placeholder")}
              value={url}
              onChange={(e) => setUrl(e.target.value)}
            />
            <button onClick={() => handleUrlImport()} disabled={loading || !url.trim()}>
              {loading ? t("source.url.importing") : t("source.url.import")}
            </button>
          </div>
        )}

        {tab === "bulk" && <BulkLinks onAdded={onSourceAdded} onError={setError} />}

        {tab === "extract" && (
          <div className="dialog-content">
            <p>{t("source.extract.intro")}</p>
            <input
              type="url"
              aria-label={t("source.url.label")}
              placeholder={t("source.extract.placeholder")}
              value={url}
              onChange={(e) => setUrl(e.target.value)}
            />
            <div className="extract-buttons">
              <button onClick={handleExtract} disabled={loading || !url.trim()}>
                {loading ? t("source.extract.working") : t("source.extract.extract")}
              </button>
              <button onClick={handleHarvest} disabled={loading || !url.trim()}>
                {loading ? t("source.extract.working") : t("source.extract.harvest")}
              </button>
            </div>
          </div>
        )}

        {tab === "catalog" && (
          <div className="dialog-content">
            <p>{t("source.catalog.intro")}</p>
            <div className="catalog-list">
              {publicFtaCatalogs().map((cat) => (
                <div key={cat.url} className="catalog-item">
                  <div>
                    <div className="catalog-name">{cat.name}</div>
                    <div className="catalog-meta">{cat.category}</div>
                  </div>
                  <button
                    disabled={loading}
                    onClick={() => handleUrlImport(cat.url, cat.name)}
                  >
                    {t("source.catalog.import")}
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

import { useEffect, useMemo, useRef, useState } from "react";
import { bulkAdd, bulkCancel, bulkCheck, bulkExtractText, bulkReadFile, errorMessage, onBulkAddResult, onBulkResult } from "../lib/tauri";
import type { FoundLinks, LinkCheck, LinkOutcome } from "../lib/types";
import { t, tn } from "../i18n/index.ts";

function statusText(outcome: LinkOutcome | undefined, running: boolean): string {
  if (!outcome) return running ? t("bulk.status.checking") : t("bulk.status.waiting");
  switch (outcome.kind) {
    case "playlist": {
      const sample = outcome.sampled > 0
        ? t("bulk.status.sample", { responding: outcome.responding, sampled: outcome.sampled })
        : t("bulk.status.noSample");
      return `${t("bulk.status.playlist", { channels: outcome.channels })} · ${sample}`;
    }
    case "stream": return t("bulk.status.stream");
    case "failed": return t("bulk.status.failed", { reason: outcome.reason });
  }
}

interface BulkLinksProps {
  /** Called once links were added, so the library can refresh. */
  onAdded: () => void;
  onError: (message: string | null) => void;
}

/** Find the links in a text, test them, and add the chosen ones as sources. */
export function BulkLinks({ onAdded, onError }: BulkLinksProps) {
  const [text, setText] = useState("");
  const [found, setFound] = useState<FoundLinks | null>(null);
  const [results, setResults] = useState<Record<string, LinkOutcome>>({});
  const [chosen, setChosen] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState<"reading" | "testing" | "adding" | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [addDone, setAddDone] = useState(0);
  const unlisten = useRef<(() => void) | null>(null);

  useEffect(() => () => { unlisten.current?.(); void bulkCancel().catch(() => {}); }, []);

  const links = useMemo(() => found?.links ?? [], [found]);
  // Counted against the current `links`, not `Object.keys(results)`: once a link is added it
  // drops out of `links` but its stale result can still linger in `results`, which would
  // otherwise make "not tested yet" go negative.
  const done = links.filter((link) => results[link]).length;
  const usable = useMemo(() => links.filter((link) => results[link] && results[link].kind !== "failed"), [links, results]);
  const failedCount = links.filter((link) => results[link]?.kind === "failed").length;

  const show = (next: FoundLinks) => {
    setFound(next);
    setResults({});
    setChosen(new Set());
    if (next.links.length > 0) {
      setMessage(null);
    } else if (next.already > 0) {
      setMessage(tn("bulk.already", next.already));
    } else {
      setMessage(t("bulk.none"));
    }
  };

  const readText = async (source: () => Promise<FoundLinks>) => {
    setBusy("reading");
    onError(null);
    try { show(await source()); } catch (failure) { onError(errorMessage(failure)); } finally { setBusy(null); }
  };

  const pickFile = async () => {
    onError(null);
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({ filters: [{ name: t("bulk.filter"), extensions: ["txt", "md", "csv", "tsv", "m3u", "m3u8", "html", "htm", "json", "log"] }] });
      if (typeof selected === "string") await readText(() => bulkReadFile(selected));
    } catch (failure) { onError(errorMessage(failure)); }
  };

  const test = async () => {
    setBusy("testing");
    onError(null);
    setResults({});
    setChosen(new Set());
    try {
      unlisten.current = await onBulkResult((check: LinkCheck) => {
        setResults((previous) => ({ ...previous, [check.url]: check.outcome }));
        if (check.outcome.kind !== "failed") setChosen((previous) => new Set(previous).add(check.url));
      });
      await bulkCheck(links);
    } catch (failure) {
      onError(errorMessage(failure));
    } finally {
      unlisten.current?.();
      unlisten.current = null;
      setBusy(null);
    }
  };

  const add = async () => {
    setBusy("adding");
    onError(null);
    setAddDone(0);
    try {
      unlisten.current = await onBulkAddResult(() => {
        setAddDone((previous) => previous + 1);
      });
      const outcome = await bulkAdd([...chosen]);
      if (outcome.failed.length > 0) {
        onError(outcome.failed.map((failedLink) => t("bulk.addFailed", { url: failedLink.url, reason: failedLink.reason })).join(" "));
        // Otherwise a link that failed to add keeps showing its old test result (e.g. "Playlist:
        // 12 channels") even though it was never added.
        setResults((previous) => {
          const next = { ...previous };
          for (const failedLink of outcome.failed) {
            next[failedLink.url] = { kind: "failed", reason: failedLink.reason };
          }
          return next;
        });
      }
      setMessage(tn("bulk.added", outcome.added.length));
      if (outcome.added.length > 0) onAdded();
      setFound({ ...(found as FoundLinks), links: links.filter((link) => !outcome.added.some((a) => a.url === link)) });
      setChosen(new Set());
    } catch (failure) {
      onError(errorMessage(failure));
    } finally {
      unlisten.current?.();
      unlisten.current = null;
      setBusy(null);
    }
  };

  const toggle = (link: string) => setChosen((previous) => {
    const next = new Set(previous);
    if (next.has(link)) next.delete(link); else next.add(link);
    return next;
  });

  return (
    <div className="dialog-content bulk-links">
      <p>{t("bulk.intro")}</p>
      <div className="extract-buttons">
        <button disabled={busy !== null} onClick={() => void pickFile()}>{t("bulk.pickFile")}</button>
      </div>
      <label htmlFor="bulk-paste">{t("bulk.pasteLabel")}</label>
      <textarea
        id="bulk-paste"
        rows={4}
        value={text}
        placeholder={t("bulk.pastePlaceholder")}
        onChange={(event) => setText(event.target.value)}
      />
      <div className="extract-buttons">
        <button disabled={busy !== null || !text.trim()} onClick={() => void readText(() => bulkExtractText(text))}>
          {busy === "reading" ? t("bulk.reading") : t("bulk.find")}
        </button>
      </div>

      {found && links.length > 0 && (
        <>
          <p role="status">
            {tn("bulk.found", links.length)}
            {found.already > 0 ? ` ${tn("bulk.already", found.already)}` : ""}
          </p>
          <div className="extract-buttons">
            {busy === "testing"
              ? <button onClick={() => void bulkCancel()}>{t("bulk.stop")}</button>
              : <button disabled={busy !== null} onClick={() => void test()}>{t("bulk.test")}</button>}
            {usable.length > 0 && busy !== "testing" && (
              <button className="secondary-button" disabled={busy !== null} onClick={() => setChosen(new Set(usable))}>{t("bulk.selectAll")}</button>
            )}
          </div>
          {(done > 0 || busy === "testing") && (
            <p className="muted-note" role="status">
              {busy === "testing" ? `${t("bulk.testing", { done, total: links.length })} · ` : ""}
              {t("bulk.summary", { ok: usable.length, failed: failedCount, waiting: links.length - done })}
            </p>
          )}
          <ul className="bulk-list">
            {links.map((link) => {
              const outcome = results[link];
              const failed = outcome?.kind === "failed";
              return (
                <li key={link} className={failed ? "bulk-failed" : undefined}>
                  <input
                    type="checkbox"
                    aria-label={t("bulk.select", { link })}
                    checked={chosen.has(link)}
                    disabled={busy !== null || !outcome || failed}
                    onChange={() => toggle(link)}
                  />
                  <div className="bulk-row-text">
                    <span className="bulk-link">{link}</span>
                    <span className="muted-note">{statusText(outcome, busy === "testing")}</span>
                  </div>
                </li>
              );
            })}
          </ul>
          <div className="extract-buttons">
            {busy === "adding"
              ? <button onClick={() => void bulkCancel()}>{t("bulk.stop")}</button>
              : <button className="primary-button" disabled={busy !== null || chosen.size === 0} onClick={() => void add()}>
                  {t("bulk.add", { count: chosen.size })}
                </button>}
          </div>
          {busy === "adding" && (
            <p className="muted-note" role="status">{t("bulk.addingProgress", { done: addDone, total: chosen.size })}</p>
          )}
        </>
      )}
      {message && <p role="status">{message}</p>}
    </div>
  );
}

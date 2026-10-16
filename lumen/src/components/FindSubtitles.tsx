import { useCallback, useEffect, useState } from "react";
import { LANGUAGES } from "../lib/languages";
import {
  errorMessage,
  subtitlesApply,
  subtitlesSearch,
  subtitlesSetOnline,
  subtitlesState,
} from "../lib/tauri";
import type { PlayingItem, SubtitleCandidate, SubtitleSearch, SubtitleState } from "../lib/types";
import { t } from "../i18n/index.ts";
import { Modal } from "./Modal";

const MAX_ROWS = 40;

function serviceLabel(id: string): string {
  return id === "gestdown" ? t("subtitles.service.gestdown") : t("subtitles.service.mirror");
}

function hintText(code: string): string | null {
  switch (code) {
    case "no_title": return t("subtitles.hint.no_title");
    case "no_episode": return t("subtitles.hint.no_episode");
    case "no_id": return t("subtitles.hint.no_id");
    default: return null;
  }
}

function describe(search: SubtitleSearch): string {
  const { title, season, episode } = search.searched;
  return season != null && episode != null
    ? t("subtitles.what.episode", { title, season, episode })
    : t("subtitles.what.title", { title });
}

interface BodyProps {
  item: PlayingItem;
  /** The subtitle language from the profile's preferences, if it is a language code. */
  preferredLanguage: string;
}

function FindSubtitlesBody({ item, preferredLanguage }: BodyProps) {
  const [state, setState] = useState<SubtitleState | null>(null);
  const [language, setLanguage] = useState(LANGUAGES.some((entry) => entry.code === preferredLanguage) ? preferredLanguage : "en");
  const [result, setResult] = useState<SubtitleSearch | null>(null);
  const [busy, setBusy] = useState(false);
  const [applying, setApplying] = useState<string | null>(null);
  const [applied, setApplied] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const search = useCallback(async (code: string) => {
    setBusy(true);
    setError(null);
    setApplied(false);
    try {
      setResult(await subtitlesSearch(item, [code]));
    } catch (failure) {
      setResult(null);
      setError(errorMessage(failure));
    } finally {
      setBusy(false);
    }
  }, [item]);

  useEffect(() => {
    let active = true;
    subtitlesState()
      .then((next) => { if (active) setState(next); })
      .catch((failure) => { if (active) setError(errorMessage(failure)); });
    return () => { active = false; };
  }, []);

  const consent = async () => {
    try {
      await subtitlesSetOnline(true);
      setState(await subtitlesState());
      await search(language);
    } catch (failure) {
      setError(errorMessage(failure));
    }
  };

  const use = async (candidate: SubtitleCandidate) => {
    setApplying(`${candidate.provider}:${candidate.id}`);
    setError(null);
    try {
      await subtitlesApply(item, candidate);
      setApplied(true);
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setApplying(null);
    }
  };

  if (state && !state.online) {
    return (
      <div className="dialog-content subtitle-finder">
        <h3 className="settings-section-title">{t("subtitles.consent.title")}</h3>
        <p>{t("subtitles.consent.body")}</p>
        <p className="muted-note">{t("subtitles.consent.warning")}</p>
        {error && <div className="dialog-error" role="alert">{error}</div>}
        <div className="settings-actions">
          <button className="primary-button" onClick={() => void consent()}>{t("subtitles.consent.accept")}</button>
        </div>
      </div>
    );
  }

  return (
    <div className="dialog-content subtitle-finder">
      {result && <h3 className="settings-section-title">{t("subtitles.heading", { what: describe(result) })}</h3>}
      <div className="subtitle-finder-controls">
        <label htmlFor="subtitle-language">{t("subtitles.language")}</label>
        <select
          id="subtitle-language"
          value={language}
          disabled={busy || !state}
          onChange={(event) => { setLanguage(event.target.value); void search(event.target.value); }}
        >
          {LANGUAGES.map((entry) => <option key={entry.code} value={entry.code}>{entry.label}</option>)}
        </select>
        <button className="primary-button" disabled={busy || !state} onClick={() => void search(language)}>
          {busy ? t("subtitles.searching") : t("subtitles.search")}
        </button>
      </div>

      {result?.hints.map((code) => {
        const text = hintText(code);
        return text ? <p key={code} className="muted-note">{text}</p> : null;
      })}
      {result?.problems.map((problem) => (
        <p key={problem.provider} className="muted-note">
          {t("subtitles.problem", { service: serviceLabel(problem.provider), message: problem.message })}
        </p>
      ))}
      {error && <div className="dialog-error" role="alert">{error}</div>}
      {applied && <p className="muted-note" role="status">{t("subtitles.applied")}</p>}

      {result && result.candidates.length === 0 && !busy && <p className="muted-note">{t("subtitles.none")}</p>}
      {result && result.candidates.length > 0 && (
        <ul className="subtitle-results">
          {result.candidates.slice(0, MAX_ROWS).map((candidate) => {
            const key = `${candidate.provider}:${candidate.id}`;
            return (
              <li key={key}>
                <div className="subtitle-row-text">
                  <strong>{candidate.release || t("subtitles.unnamed")}</strong>
                  <span className="muted-note">
                    {serviceLabel(candidate.provider)}
                    {candidate.hashMatch ? ` · ${t("subtitles.exactFile")}` : ""}
                    {candidate.hearingImpaired ? ` · ${t("subtitles.hearingImpaired")}` : ""}
                    {candidate.downloads != null ? ` · ${t("subtitles.downloads", { count: candidate.downloads })}` : ""}
                  </span>
                </div>
                <button className="secondary-button" disabled={applying !== null} onClick={() => void use(candidate)}>
                  {applying === key ? t("subtitles.using") : t("subtitles.use")}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

interface FindSubtitlesProps extends Omit<BodyProps, "item"> {
  open: boolean;
  item: PlayingItem | null;
  onClose: () => void;
}

/** Looks subtitles up for what is playing and adds the one the user picks. Asks before the first search. */
export function FindSubtitles({ open, item, preferredLanguage, onClose }: FindSubtitlesProps) {
  return (
    <Modal open={open} onClose={onClose} title={t("subtitles.title")} className="subtitle-dialog">
      {item && <FindSubtitlesBody item={item} preferredLanguage={preferredLanguage} />}
    </Modal>
  );
}

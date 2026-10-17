import { useRef, useState } from "react";
import type { Channel } from "../lib/types";
import { CANONICAL_GENRES, contentKind, genresOf, type ContentKind } from "../lib/discovery";
import { t } from "../i18n/index.ts";

interface CategoryOverridePickerProps {
  channel: Channel;
  onSave: (kind: ContentKind | null, genres: string[] | null) => Promise<void>;
}

/**
 * Lets the user correct a title's Kind and Genre(s) when TMDB never matched it — playlists carry
 * no such field, so Lumen otherwise only guesses one from the group/name text. The caller must
 * not render this once TMDB has actually matched the title; TMDB stays authoritative there.
 */
export function CategoryOverridePicker({ channel, onSave }: CategoryOverridePickerProps) {
  const [kind, setKind] = useState<ContentKind>(contentKind(channel));
  const [genres, setGenres] = useState<Set<string>>(
    () => new Set(genresOf(channel).names.filter((name) => CANONICAL_GENRES.includes(name))),
  );
  const [error, setError] = useState<string | null>(null);
  // Rapidly toggling two chips can leave two saves in flight at once. If the older one rejects
  // after the newer one already applied its own change, the older call's rollback must not
  // overwrite it — so a rollback only runs when no newer call has started since.
  const latestSave = useRef(0);

  const save = async (nextKind: ContentKind, nextGenres: Set<string>) => {
    const callId = ++latestSave.current;
    const previousKind = kind;
    const previousGenres = genres;
    setKind(nextKind);
    setGenres(nextGenres);
    setError(null);
    try {
      await onSave(nextKind, nextGenres.size ? [...nextGenres] : null);
    } catch {
      if (latestSave.current !== callId) return;
      setKind(previousKind);
      setGenres(previousGenres);
      setError(t("app.details.category.saveFailed"));
    }
  };

  const toggleGenre = (name: string) => {
    const next = new Set(genres);
    if (next.has(name)) next.delete(name); else next.add(name);
    void save(kind, next);
  };

  return (
    <div className="category-override">
      <p className="muted-note">{t("app.details.category.hint")}</p>
      <label className="category-override-field">
        <span>{t("app.details.category.kindLabel")}</span>
        <select
          aria-label={t("app.details.category.kindAria")}
          value={kind}
          onChange={(event) => void save(event.target.value as ContentKind, genres)}
        >
          <option value="movie">{t("app.details.category.kindMovie")}</option>
          <option value="series">{t("app.details.category.kindSeries")}</option>
          <option value="live">{t("app.details.category.kindLive")}</option>
        </select>
      </label>
      <div className="category-override-genres" role="group" aria-label={t("app.details.category.genresLabel")}>
        {CANONICAL_GENRES.map((name) => (
          <button
            key={name}
            type="button"
            className={`facet-chip ${genres.has(name) ? "active" : ""}`}
            aria-pressed={genres.has(name)}
            onClick={() => toggleGenre(name)}
          >
            {name}
          </button>
        ))}
      </div>
      {error && <p className="dialog-error" role="alert">{error}</p>}
    </div>
  );
}

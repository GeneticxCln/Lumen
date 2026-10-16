import { Fragment, useEffect, useRef, useState } from "react";
import { Icon } from "./Icon";
import { MediaShelf } from "./MediaShelf";
import { errorMessage, getSearchProviders, getSettings, searchEngine, setSearchProviders, setSetting } from "../lib/tauri";
import { matchesLanguage } from "../lib/language";
import { t, tRich } from "../i18n/index.ts";
import { Rich } from "../i18n/Rich.tsx";
import type { Channel, SearchProviderConfig, SearchResponse } from "../lib/types";

interface SearchEnginePageProps {
  query: string;
  favorites: Set<number>;
  compact: boolean;
  onPlay: (channel: Channel) => void;
  onPlayExternal: (url: string, title: string, headers: Record<string, string>) => void;
  onDetails: (channel: Channel) => void;
  onFavorite: (id: number) => void;
  onRefresh: () => void;
}

function configToJson(provider: SearchProviderConfig): string {
  const { token: _token, ...safeConfig } = provider;
  return JSON.stringify(safeConfig, null, 2);
}

const SEARCH_LANGUAGE_SETTING = "search_preferred_language";
const SEARCH_HIDE_OTHER_LANGUAGES_SETTING = "search_hide_other_languages";

const PREFERRED_LANGUAGES = [
  { code: "en", label: "searchEngine.language.en" },
  { code: "fr", label: "searchEngine.language.fr" },
  { code: "de", label: "searchEngine.language.de" },
  { code: "es", label: "searchEngine.language.es" },
  { code: "it", label: "searchEngine.language.it" },
  { code: "pt", label: "searchEngine.language.pt" },
  { code: "nl", label: "searchEngine.language.nl" },
  { code: "ru", label: "searchEngine.language.ru" },
  { code: "tr", label: "searchEngine.language.tr" },
  { code: "ar", label: "searchEngine.language.ar" },
  { code: "hi", label: "searchEngine.language.hi" },
  { code: "ja", label: "searchEngine.language.ja" },
  { code: "ko", label: "searchEngine.language.ko" },
  { code: "zh", label: "searchEngine.language.zh" },
] as const;

export function SearchEnginePage({ query, favorites, compact, onPlay, onPlayExternal, onDetails, onFavorite, onRefresh }: SearchEnginePageProps) {
  const [response, setResponse] = useState<SearchResponse | null>(null);
  const [providers, setProviders] = useState<SearchProviderConfig[]>([]);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [language, setLanguage] = useState("");
  const [hideOtherLanguages, setHideOtherLanguages] = useState(false);
  const lastSearch = useRef(0);

  useEffect(() => {
    let active = true;
    getSettings().then((settings) => {
      const stored = settings.search_preferred_language;
      if (active && PREFERRED_LANGUAGES.some((option) => option.code === stored)) setLanguage(stored);
      if (active) setHideOtherLanguages(settings.search_hide_other_languages === "true");
    }).catch((failure) => { if (active) setError(errorMessage(failure)); });
    return () => { active = false; };
  }, []);

  useEffect(() => {
    let active = true;
    getSearchProviders().then(({ providers: current }) => {
      if (!active) return;
      setProviders(current);
      setDraft(current.length ? current.map(configToJson).join("\n\n") : "");
    }).catch((failure) => { if (active) setError(errorMessage(failure)); });
    return () => { active = false; };
  }, []);

  useEffect(() => {
    const term = query.trim();
    if (term.length < 2) { setResponse(null); return; }
    let active = true;
    const sequence = ++lastSearch.current;
    const timer = window.setTimeout(() => {
      setBusy(true);
      setError(null);
      searchEngine(term, language || undefined).then((result) => {
        if (active && sequence === lastSearch.current) setResponse(result);
      }).catch((failure) => {
        if (active && sequence === lastSearch.current) setError(errorMessage(failure));
      }).finally(() => { if (active && sequence === lastSearch.current) setBusy(false); });
    }, 280);
    return () => { active = false; window.clearTimeout(timer); };
  }, [query, language]);

  const saveProviders = async () => {
    setSaving(true); setError(null); setNotice(null);
    try {
      const parsed = draft.trim() ? JSON.parse(`[${draft.replace(/}\s*\n+\s*{/g, "},{")}]`) : [];
      if (!Array.isArray(parsed)) throw new Error(t("searchEngine.error.notList"));
      if (parsed.length > 12) throw new Error(t("searchEngine.error.tooMany"));
      for (const provider of parsed) {
        if (!provider || typeof provider !== "object" || Array.isArray(provider)) throw new Error(t("searchEngine.error.notObject"));
      }
      const { providers: savedProviders } = await setSearchProviders(parsed);
      setProviders(savedProviders);
      setDraft(savedProviders.map(configToJson).join("\n\n"));
      setNotice(t("searchEngine.saved"));
      setResponse(null);
    } catch (failure) {
      setError(failure instanceof SyntaxError ? t("searchEngine.error.syntax") : errorMessage(failure));
    } finally { setSaving(false); }
  };

  const visible = (response?.results || []).filter((result) => !hideOtherLanguages || matchesLanguage(result, language));
  const hiddenCount = (response?.results.length || 0) - visible.length;
  const channels: Channel[] = visible.filter((result) => result.channel_id !== undefined).map((result) => ({
    id: result.channel_id!, playlist_id: result.playlist_id || 0, name: result.title, url: result.url,
    group_title: result.group_title || result.providers.join(" · "),
    extra_headers: JSON.stringify(result.headers || {}),
    last_ok: result.available ? Math.floor(Date.now() / 1000) : undefined,
    last_error: result.available === false ? (result.status ? t("searchEngine.httpStatus", { status: result.status }) : t("searchEngine.unavailable")) : undefined,
    tmdb_type: result.media_type === "series" ? "tv" : result.media_type === "movie" ? "movie" : undefined,
    release_year: result.year || undefined,
  }));

  const summaryParts = (searched: string) => {
    const values = { count: visible.length, query: searched };
    return hiddenCount > 0 ? tRich("searchEngine.summaryHidden", { ...values, hidden: hiddenCount }) : tRich("searchEngine.summary", values);
  };

  return <div className="engine-search-page">
    <section className="settings-panel search-engine-panel">
      <header className="settings-panel-head">
        <div className="settings-breadcrumb">{t("searchEngine.breadcrumbApp")} <Icon name="chevron" size={12} /> {t("searchEngine.title")}</div>
        <div className="settings-title-row"><span className="settings-title-icon"><Icon name="search" size={23} /></span><div><h2>{t("searchEngine.title")}</h2><p>{t("searchEngine.subtitle")}</p></div></div>
      </header>
      <p className="settings-intro"><Rich id="searchEngine.intro" tags={{ code: (text) => <code>{text}</code> }} /></p>
      <div className="search-provider-list">
        <div className="settings-source"><div><h3>{t("searchEngine.library.name")}</h3><p>{t("searchEngine.library.note")}</p></div><span className="match-label">{t("searchEngine.library.alwaysOn")}</span></div>
        {providers.map((provider) => <div className="settings-source" key={provider.id}><div><h3>{provider.name}</h3><p>{provider.endpoint}</p></div><span className="match-label">{t("searchEngine.jsonBadge")}</span></div>)}
      </div>
      <div className="form-group">
        <label htmlFor="search-providers">{t("searchEngine.providersLabel")}</label>
        <textarea id="search-providers" rows={12} spellCheck={false} value={draft} onChange={(event) => setDraft(event.target.value)} placeholder={t("searchEngine.providersExample", { open: "{", close: "}" })} />
        <small>{t("searchEngine.providersHelp")}</small>
      </div>
      {error && <div className="dialog-error" role="alert">{error}</div>}
      {notice && <p className="saved-badge" role="status">{notice}</p>}
      <div className="settings-actions"><button className="primary-button" disabled={saving} onClick={() => void saveProviders()}>{saving ? t("searchEngine.saving") : t("searchEngine.save")}</button></div>
    </section>

    <div className="browse-toolbar search-filters">
      <div className="form-group">
        <label htmlFor="search-language">{t("searchEngine.language.label")}</label>
        <select id="search-language" value={language} onChange={(event) => {
          const next = event.target.value;
          setLanguage(next);
          setError(null);
          setSetting(SEARCH_LANGUAGE_SETTING, next).catch((failure) => setError(errorMessage(failure)));
        }}>
          <option value="">{t("searchEngine.language.any")}</option>
          {PREFERRED_LANGUAGES.map((option) => <option key={option.code} value={option.code}>{t(option.label)}</option>)}
        </select>
        <small>{hideOtherLanguages && language ? t("searchEngine.language.hiddenNote") : t("searchEngine.language.rankNote")}</small>
      </div>
      <label className="search-toggle">
        <input type="checkbox" checked={hideOtherLanguages} disabled={!language} onChange={(event) => {
          const hide = event.target.checked;
          setHideOtherLanguages(hide);
          setError(null);
          setSetting(SEARCH_HIDE_OTHER_LANGUAGES_SETTING, hide ? "true" : "false").catch((failure) => setError(errorMessage(failure)));
        }} />
        {t("searchEngine.hideOther")}
      </label>
    </div>

    {query.trim().length < 2
      ? <div className="empty-state"><div className="empty-symbol"><Icon name="search" size={32} /></div><h2>{t("searchEngine.prompt.title")}</h2><p>{t("searchEngine.prompt.body")}</p></div>
      : busy && !response
        ? <p className="loading" role="status">{t("searchEngine.searching")}</p>
        : response && <>
          <div className="browse-toolbar"><p className="browse-summary" role="status">{summaryParts(response.query).map((part, index) => part.tag === "strong" ? <strong key={index}>{part.text}</strong> : <Fragment key={index}>{part.text}</Fragment>)}</p></div>
          {response.providers.map((provider) => <p className="muted-note" key={provider.id} role="status">{provider.status === "ok" ? t("searchEngine.providerOk", { name: provider.name, count: provider.count }) : t("searchEngine.providerProblem", { name: provider.name, status: provider.status, error: provider.error || t("searchEngine.providerUnavailable") })}</p>)}
          {channels.length > 0 ? <MediaShelf title={t("searchEngine.shelf.title")} subtitle={t("searchEngine.shelf.subtitle")} channels={channels} favorites={favorites} onPlay={onPlay} onDetails={onDetails} onFavorite={onFavorite} compact={compact} /> : null}
          {visible.filter((result) => result.channel_id === undefined).length > 0 && <section className="search-remote-results" aria-label={t("searchEngine.external.region")}><h2>{t("searchEngine.external.title")}</h2>{visible.filter((result) => result.channel_id === undefined).map((result) => <article className="settings-source" key={result.id}><div><h3>{result.year ? t("searchEngine.resultTitle", { title: result.title, year: result.year }) : result.title}</h3><p>{t("searchEngine.resultMeta", { providers: result.providers.join(" · "), details: [result.quality, result.language, result.content_type, result.available === false ? t("searchEngine.unavailable") : result.available ? t("searchEngine.available") : t("searchEngine.notChecked")].filter(Boolean).join(" · ") })}</p><p className="muted-note">{result.url}</p></div><button className="secondary-button" disabled={!result.url} onClick={() => onPlayExternal(result.url, result.title, result.headers || {})}>{t("transport.play")}</button></article>)}</section>}
          {!visible.length && <div className="empty-state"><h2>{t("searchEngine.noMatches.title", { query: response.query })}</h2><p>{hiddenCount > 0 ? t("searchEngine.noMatches.hidden", { count: hiddenCount }) : t("searchEngine.noMatches.check")}</p></div>}
        </>}
    <button className="secondary-button" onClick={onRefresh}>{t("searchEngine.refresh")}</button>
  </div>;
}

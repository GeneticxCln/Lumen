import { useEffect, useRef, useState } from "react";
import { Icon } from "./Icon";
import { MediaShelf } from "./MediaShelf";
import { errorMessage, getSearchProviders, getSettings, searchEngine, setSearchProviders, setSetting } from "../lib/tauri";
import { matchesLanguage } from "../lib/language";
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

const PREFERRED_LANGUAGES: Array<{ code: string; label: string }> = [
  { code: "en", label: "English" },
  { code: "fr", label: "French" },
  { code: "de", label: "German" },
  { code: "es", label: "Spanish" },
  { code: "it", label: "Italian" },
  { code: "pt", label: "Portuguese" },
  { code: "nl", label: "Dutch" },
  { code: "ru", label: "Russian" },
  { code: "tr", label: "Turkish" },
  { code: "ar", label: "Arabic" },
  { code: "hi", label: "Hindi" },
  { code: "ja", label: "Japanese" },
  { code: "ko", label: "Korean" },
  { code: "zh", label: "Chinese" },
];

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
      if (!Array.isArray(parsed)) throw new Error("Providers must be one JSON object per block.");
      if (parsed.length > 12) throw new Error("At most 12 external providers can be enabled.");
      for (const provider of parsed) {
        if (!provider || typeof provider !== "object" || Array.isArray(provider)) throw new Error("Each provider must be a JSON object.");
      }
      const { providers: savedProviders } = await setSearchProviders(parsed);
      setProviders(savedProviders);
      setDraft(savedProviders.map(configToJson).join("\n\n"));
      setNotice("Provider settings saved on this device.");
      setResponse(null);
    } catch (failure) {
      setError(failure instanceof SyntaxError ? "Check the provider JSON syntax. Separate provider objects with a blank line." : errorMessage(failure));
    } finally { setSaving(false); }
  };

  const visible = (response?.results || []).filter((result) => !hideOtherLanguages || matchesLanguage(result, language));
  const hiddenCount = (response?.results.length || 0) - visible.length;
  const channels: Channel[] = visible.filter((result) => result.channel_id !== undefined).map((result) => ({
    id: result.channel_id!, playlist_id: result.playlist_id || 0, name: result.title, url: result.url,
    group_title: result.group_title || result.providers.join(" · "),
    extra_headers: JSON.stringify(result.headers || {}),
    last_ok: result.available ? Math.floor(Date.now() / 1000) : undefined,
    last_error: result.available === false ? (result.status ? `HTTP ${result.status}` : "Unavailable") : undefined,
    tmdb_type: result.media_type === "series" ? "tv" : result.media_type === "movie" ? "movie" : undefined,
    release_year: result.year || undefined,
  }));

  return <div className="engine-search-page">
    <section className="settings-panel search-engine-panel">
      <header className="settings-panel-head">
        <div className="settings-breadcrumb">Lumen <Icon name="chevron" size={12} /> Search engine</div>
        <div className="settings-title-row"><span className="settings-title-icon"><Icon name="search" size={23} /></span><div><h2>Search engine</h2><p>Search your library and configured JSON providers.</p></div></div>
      </header>
      <p className="settings-intro">External providers are user-configured JSON endpoints. Lumen sends <code>?q=your search</code> and maps the JSON fields you specify. Search the endpoint’s own terms and follow its rules.</p>
      <div className="search-provider-list">
        <div className="settings-source"><div><h3>My library</h3><p>Built in · searches the playlists you added</p></div><span className="match-label">Always on</span></div>
        {providers.map((provider) => <div className="settings-source" key={provider.id}><div><h3>{provider.name}</h3><p>{provider.endpoint}</p></div><span className="match-label">JSON</span></div>)}
      </div>
      <div className="form-group">
        <label htmlFor="search-providers">External providers · one JSON object per block</label>
        <textarea id="search-providers" rows={12} spellCheck={false} value={draft} onChange={(event) => setDraft(event.target.value)} placeholder={'{\n  "id": "catalog",\n  "name": "My catalog",\n  "endpoint": "https://example.org/search",\n  "token": "optional bearer token",\n  "results_path": "results",\n  "title_field": "title",\n  "url_field": "url",\n  "aliases_field": "aliases",\n  "year_field": "year",\n  "quality_field": "quality",\n  "language_field": "language"\n}'} />
        <small>Provider settings are saved on this device. Tokens are kept in the OS credential store, masked here, and preserved when omitted; enter an empty token string to clear one. Tokens are sent only to the provider endpoint, never returned in the provider list. JSON data must have an array at results_path; each item must have a title and playable HTTP(S) URL.</small>
      </div>
      {error && <div className="dialog-error" role="alert">{error}</div>}
      {notice && <p className="saved-badge" role="status">{notice}</p>}
      <div className="settings-actions"><button className="primary-button" disabled={saving} onClick={() => void saveProviders()}>{saving ? "Saving…" : "Save providers"}</button></div>
    </section>

    <div className="browse-toolbar search-filters">
      <div className="form-group">
        <label htmlFor="search-language">Preferred language</label>
        <select id="search-language" value={language} onChange={(event) => {
          const next = event.target.value;
          setLanguage(next);
          setError(null);
          setSetting(SEARCH_LANGUAGE_SETTING, next).catch((failure) => setError(errorMessage(failure)));
        }}>
          <option value="">Any language</option>
          {PREFERRED_LANGUAGES.map((option) => <option key={option.code} value={option.code}>{option.label}</option>)}
        </select>
        <small>{hideOtherLanguages && language ? "Results in other languages are hidden. Items with no language label always show." : "Matching results rank first. Other languages still appear."}</small>
      </div>
      <label className="search-toggle">
        <input type="checkbox" checked={hideOtherLanguages} disabled={!language} onChange={(event) => {
          const hide = event.target.checked;
          setHideOtherLanguages(hide);
          setError(null);
          setSetting(SEARCH_HIDE_OTHER_LANGUAGES_SETTING, hide ? "true" : "false").catch((failure) => setError(errorMessage(failure)));
        }} />
        Hide other languages
      </label>
    </div>

    {query.trim().length < 2
      ? <div className="empty-state"><div className="empty-symbol"><Icon name="search" size={32} /></div><h2>Search your library and providers.</h2><p>Type at least two characters in the search bar. Manage external provider endpoints above.</p></div>
      : busy && !response
        ? <p className="loading" role="status">Searching providers…</p>
        : response && <>
          <div className="browse-toolbar"><p className="browse-summary" role="status"><strong>{visible.length}</strong> results for “{response.query}”{hiddenCount > 0 ? ` · ${hiddenCount} hidden as other languages` : ""}</p></div>
          {response.providers.map((provider) => <p className="muted-note" key={provider.id} role="status">{provider.name}: {provider.status === "ok" ? `${provider.count} results` : `${provider.status} · ${provider.error || "Provider unavailable"}`}</p>)}
          {channels.length > 0 ? <MediaShelf title="From your library" subtitle="Playable matches from sources on this device." channels={channels} favorites={favorites} onPlay={onPlay} onDetails={onDetails} onFavorite={onFavorite} compact={compact} /> : null}
          {visible.filter((result) => result.channel_id === undefined).length > 0 && <section className="search-remote-results" aria-label="External search results"><h2>External provider results</h2>{visible.filter((result) => result.channel_id === undefined).map((result) => <article className="settings-source" key={result.id}><div><h3>{result.title}{result.year ? ` (${result.year})` : ""}</h3><p>{result.providers.join(" · ")} · {[result.quality, result.language, result.content_type, result.available === false ? "Unavailable" : result.available ? "Available" : "Not checked"].filter(Boolean).join(" · ")}</p><p className="muted-note">{result.url}</p></div><button className="secondary-button" disabled={!result.url} onClick={() => onPlayExternal(result.url, result.title, result.headers || {})}>Play</button></article>)}</section>}
          {!visible.length && <div className="empty-state"><h2>No matches for “{response.query}”.</h2><p>{hiddenCount > 0 ? `${hiddenCount} results are in other languages. Turn off “Hide other languages” to see them.` : "Check provider status above or try another title."}</p></div>}
        </>}
    <button className="secondary-button" onClick={onRefresh}>Refresh library</button>
  </div>;
}

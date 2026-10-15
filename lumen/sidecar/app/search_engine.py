from __future__ import annotations

import asyncio
import copy
import hashlib
import json
import re
import time
from collections import OrderedDict
from dataclasses import dataclass
from typing import Any, Protocol
from urllib.parse import urlparse

import httpx

from .extract import validate_http_url
from .probe import probe_urls

MAX_PROVIDERS = 12
MAX_PROVIDER_RESULTS = 40
MAX_SEARCH_RESULTS = 80
MAX_QUERY_LENGTH = 160
PROVIDER_TIMEOUT_SECONDS = 8.0
MAX_PROVIDER_TIMEOUT_SECONDS = 15.0
SEARCH_CACHE_SECONDS = 25.0
MAX_CACHE_ENTRIES = 128
MAX_CONCURRENT_SEARCHES = 4

_QUALITY_RE = re.compile(r"\b(4320|2160|1440|1080|720|576|480|360)\s*p?\b|\b(8k|4k|uhd|fhd|hd)\b", re.I)
_QUALITY_RANK = {"4320p": 8, "8k": 8, "2160p": 7, "4k": 7, "1440p": 6, "1080p": 5, "fhd": 5, "720p": 4, "hd": 3, "576p": 3, "480p": 2, "360p": 1}


def normalize_text(value: Any) -> str:
    if not isinstance(value, str):
        return ""
    return " ".join(re.sub(r"[^a-z0-9]+", " ", value.casefold()).split())


def _field(row: dict[str, Any], path: str) -> Any:
    value: Any = row
    for part in path.split("."):
        if not isinstance(value, dict):
            return None
        value = value.get(part)
    return value


def _aliases(value: Any) -> list[str]:
    if isinstance(value, str):
        values = re.split(r"[;,|]", value)
    elif isinstance(value, list):
        values = value
    else:
        return []
    return list(dict.fromkeys(text.strip() for text in values if isinstance(text, str) and text.strip()))[:20]


def _metadata_value(value: Any, maximum: int = 80) -> str | None:
    if not isinstance(value, (str, int, float)) or isinstance(value, bool):
        return None
    text = str(value).strip()
    return text[:maximum] or None


def _safe_headers(value: Any) -> dict[str, str]:
    if not isinstance(value, dict):
        return {}
    headers: dict[str, str] = {}
    for key, item in list(value.items())[:20]:
        if not isinstance(key, str) or not isinstance(item, str):
            continue
        if re.fullmatch(r"[!#$%&'*+.^_`|~0-9A-Za-z-]{1,80}", key) and "\r" not in item and "\n" not in item:
            headers[key] = item[:2048]
    return headers


def _validate_provider(config: dict[str, Any]) -> dict[str, Any]:
    provider_id = str(config.get("id", "")).strip()
    name = str(config.get("name", "")).strip()
    endpoint = str(config.get("endpoint", "")).strip()
    try:
        validate_http_url(endpoint)
    except ValueError as error:
        raise ValueError(f"{name or 'Provider'}: {error}") from error
    parsed = urlparse(endpoint)
    if parsed.username or parsed.password or parsed.fragment:
        raise ValueError(f"{name or 'Provider'}: endpoint must not contain credentials or a fragment")
    if not provider_id or len(provider_id) > 80:
        raise ValueError("Provider id is missing or too long")
    if not name or len(name) > 80:
        raise ValueError("Provider name must be between 1 and 80 characters")
    timeout_value = config.get("timeout", PROVIDER_TIMEOUT_SECONDS)
    try:
        timeout_seconds = min(MAX_PROVIDER_TIMEOUT_SECONDS, max(1.0, float(timeout_value)))
    except (TypeError, ValueError):
        timeout_seconds = PROVIDER_TIMEOUT_SECONDS
    return {
        "id": provider_id,
        "name": name,
        "endpoint": endpoint,
        "token": str(config.get("token", ""))[:4096],
        "timeout": timeout_seconds,
        "results_path": str(config.get("results_path", "results"))[:120],
        "title_field": str(config.get("title_field", "title"))[:120],
        "url_field": str(config.get("url_field", "url"))[:120],
        "aliases_field": str(config.get("aliases_field", "aliases"))[:120],
        "year_field": str(config.get("year_field", "year"))[:120],
        "media_type_field": str(config.get("media_type_field", "media_type"))[:120],
        "quality_field": str(config.get("quality_field", "quality"))[:120],
        "language_field": str(config.get("language_field", "language"))[:120],
        "headers_field": str(config.get("headers_field", "headers"))[:120],
    }


class SearchProvider(Protocol):
    provider_id: str
    name: str

    async def search(self, query: str) -> list[dict[str, Any]]: ...


@dataclass
class LibraryProvider:
    entries: list[dict[str, Any]]
    provider_id: str = "library"
    name: str = "My library"

    async def search(self, query: str) -> list[dict[str, Any]]:
        terms = normalize_text(query).split()
        found: list[dict[str, Any]] = []
        for entry in self.entries[:5000]:
            if not isinstance(entry, dict):
                continue
            title = str(entry.get("name") or entry.get("title") or "").strip()
            url = str(entry.get("url") or "").strip()
            if not title or not url:
                continue
            aliases = _aliases([entry.get("group_title"), entry.get("tvg_id"), *(_aliases(entry.get("aliases")))])
            searchable = normalize_text(" ".join([title, *aliases]))
            if terms and not all(term in searchable for term in terms):
                continue
            try:
                validated_url = validate_http_url(url)
            except ValueError:
                continue
            raw_headers = entry.get("extra_headers")
            if isinstance(raw_headers, str):
                try:
                    raw_headers = json.loads(raw_headers)
                except (json.JSONDecodeError, TypeError):
                    raw_headers = {}
            raw_type = entry.get("tmdb_type")
            media_type = {"movie": "movie", "tv": "series", "series": "series"}.get(raw_type)
            found.append({
                "id": f"library:{entry.get('id', len(found))}",
                "title": title[:240],
                "url": validated_url,
                "provider": self.name,
                "aliases": aliases,
                "year": entry.get("release_year") if isinstance(entry.get("release_year"), int) else None,
                "media_type": media_type,
                "quality": None,
                "language": None,
                "headers": _safe_headers(raw_headers),
                "channel_id": entry.get("id"),
                "playlist_id": entry.get("playlist_id"),
                "group_title": entry.get("group_title"),
            })
        return found


class UserJsonProvider:
    def __init__(self, config: dict[str, Any], client: httpx.AsyncClient):
        self.config = _validate_provider(config)
        self.client = client
        self.provider_id = self.config["id"]
        self.name = self.config["name"]

    async def search(self, query: str) -> list[dict[str, Any]]:
        headers = {"Accept": "application/json", "User-Agent": "Lumen/0.1"}
        if self.config["token"]:
            headers["Authorization"] = f"Bearer {self.config['token']}"
        response = await self.client.get(
            self.config["endpoint"],
            params={"q": query},
            headers=headers,
            timeout=self.config["timeout"],
        )
        response.raise_for_status()
        payload = response.json()
        rows = _field(payload, self.config["results_path"]) if self.config["results_path"] else payload
        if not isinstance(rows, list):
            raise ValueError(f"{self.name}: response field '{self.config['results_path']}' must be an array")

        results: list[dict[str, Any]] = []
        for index, row in enumerate(rows[:MAX_PROVIDER_RESULTS]):
            if not isinstance(row, dict):
                continue
            title = _field(row, self.config["title_field"])
            url = _field(row, self.config["url_field"])
            if not isinstance(title, str) or not title.strip() or not isinstance(url, str):
                continue
            try:
                url = validate_http_url(url)
            except ValueError:
                continue
            raw_year = _field(row, self.config["year_field"])
            try:
                year = int(raw_year) if raw_year is not None else None
                if year is not None and not 1870 <= year <= 2200:
                    year = None
            except (TypeError, ValueError):
                year = None
            media_type = _metadata_value(_field(row, self.config["media_type_field"]), 32)
            if media_type:
                media_type = {"tv": "series", "show": "series", "film": "movie"}.get(media_type.casefold(), media_type.casefold())
            results.append({
                "id": f"{self.provider_id}:{row.get('id', index)}",
                "title": title.strip()[:240],
                "url": url,
                "provider": self.name,
                "aliases": _aliases(_field(row, self.config["aliases_field"])),
                "year": year,
                "media_type": media_type,
                "quality": _metadata_value(_field(row, self.config["quality_field"]), 40),
                "language": _metadata_value(_field(row, self.config["language_field"]), 40),
                "headers": _safe_headers(_field(row, self.config["headers_field"])),
            })
        return results


class ProviderManager:
    """Build the built-in library provider and configured user JSON providers."""

    def __init__(self, client: httpx.AsyncClient):
        self.client = client

    def build(self, library: list[dict[str, Any]], configs: list[dict[str, Any]]) -> list[SearchProvider]:
        providers: list[SearchProvider] = [LibraryProvider(library)]
        if len(configs) > MAX_PROVIDERS:
            raise ValueError(f"At most {MAX_PROVIDERS} external providers can be enabled")
        seen_ids = {"library"}
        for raw in configs:
            if not isinstance(raw, dict):
                continue
            config = _validate_provider(raw)
            if config["id"] in seen_ids:
                raise ValueError(f"Duplicate provider id: {config['id']}")
            seen_ids.add(config["id"])
            providers.append(UserJsonProvider(config, self.client))
        return providers


def _score(result: dict[str, Any], query: str) -> int:
    normalized_query = normalize_text(query)
    if not normalized_query:
        return 0
    terms = normalized_query.split()
    title = normalize_text(result.get("title"))
    aliases = [normalize_text(alias) for alias in result.get("aliases", [])]
    haystack = " ".join([title, *aliases])
    if title == normalized_query:
        return 100
    if normalized_query in title:
        return 90
    if all(term in haystack for term in terms):
        return 75 + sum(3 for term in terms if term in title)
    return sum(1 for term in terms if term in haystack)


def _quality(result: dict[str, Any]) -> str | None:
    explicit = _metadata_value(result.get("quality"), 40)
    if explicit:
        return explicit
    match = _QUALITY_RE.search(result.get("title", ""))
    if not match:
        return None
    value = (match.group(1) or match.group(2) or "").casefold()
    return f"{value}p" if value.isdigit() else value


def _identity(result: dict[str, Any]) -> set[str]:
    return {normalize_text(result.get("title")), *(normalize_text(alias) for alias in result.get("aliases", []))} - {""}


def _same_title(left: dict[str, Any], right: dict[str, Any]) -> bool:
    if left.get("year") and right.get("year") and left["year"] != right["year"]:
        return False
    left_type, right_type = left.get("media_type"), right.get("media_type")
    if left_type and right_type and left_type != right_type:
        return False
    return bool(_identity(left) & _identity(right))


def _deduplicate(results: list[dict[str, Any]], query: str) -> list[dict[str, Any]]:
    merged: list[dict[str, Any]] = []
    for result in results:
        result["score"] = _score(result, query)
        if result["score"] <= 0:
            continue
        result["quality"] = _quality(result)
        duplicate = next((item for item in merged if _same_title(item, result) or item["url"].split("#", 1)[0] == result["url"].split("#", 1)[0]), None)
        if duplicate is None:
            result["providers"] = [result["provider"]]
            merged.append(result)
            continue
        duplicate["providers"] = list(dict.fromkeys([*duplicate["providers"], result["provider"]]))
        duplicate["aliases"] = list(dict.fromkeys([*duplicate["aliases"], *result["aliases"]]))[:30]
        if not duplicate.get("year"):
            duplicate["year"] = result.get("year")
        for field in ("media_type", "quality", "language", "content_type", "status", "available"):
            if duplicate.get(field) in (None, False) and result.get(field) not in (None, False):
                duplicate[field] = result[field]
        if not duplicate.get("headers"):
            duplicate["headers"] = result.get("headers") or {}
        if result["score"] > duplicate["score"]:
            # Keep local channel identity if the same item is available from the library.
            channel_id, playlist_id = duplicate.get("channel_id"), duplicate.get("playlist_id")
            duplicate.update(result)
            if channel_id is not None:
                duplicate["channel_id"] = channel_id
                duplicate["playlist_id"] = playlist_id
        duplicate["score"] = max(duplicate["score"], result["score"])
    return merged


class SearchEngine:
    """Bounded asynchronous search with timeouts, failure isolation, and short TTL cache."""

    def __init__(self):
        self._cache: OrderedDict[str, tuple[float, dict[str, Any]]] = OrderedDict()
        self._search_slots = asyncio.Semaphore(MAX_CONCURRENT_SEARCHES)
        self._client: httpx.AsyncClient | None = None

    async def _http_client(self) -> httpx.AsyncClient:
        if self._client is None or self._client.is_closed:
            self._client = httpx.AsyncClient(follow_redirects=True, timeout=PROVIDER_TIMEOUT_SECONDS)
        return self._client

    @staticmethod
    def _cache_key(query: str, library: list[dict[str, Any]], configs: list[dict[str, Any]], check_availability: bool) -> str:
        # Hash the provider configuration so credentials never become cache-key text.
        config_hash = hashlib.sha256(json.dumps(configs, sort_keys=True, default=str).encode()).hexdigest()
        library_hash = hashlib.sha256(json.dumps(library, sort_keys=True, default=str).encode()).hexdigest()
        return f"{normalize_text(query)}:{config_hash}:{library_hash}:{int(check_availability)}"

    async def search(
        self,
        query: str,
        library: list[dict[str, Any]],
        configs: list[dict[str, Any]],
        *,
        check_availability: bool = True,
    ) -> dict[str, Any]:
        query = query.strip()
        if len(query) < 2:
            return {"query": query, "results": [], "providers": []}
        if len(query) > MAX_QUERY_LENGTH:
            raise ValueError(f"Search query must be at most {MAX_QUERY_LENGTH} characters")
        if len(library) > 5000:
            raise ValueError("Library search payload exceeds 5000 entries")

        key = self._cache_key(query, library, configs, check_availability)
        now = time.monotonic()
        cached = self._cache.get(key)
        if cached and cached[0] > now:
            self._cache.move_to_end(key)
            return copy.deepcopy(cached[1])
        if cached:
            del self._cache[key]

        async with self._search_slots:
            client = await self._http_client()
            manager = ProviderManager(client)
            providers = manager.build(library, configs)
            async def run_provider(provider: SearchProvider) -> tuple[list[dict[str, Any]], dict[str, Any]]:
                try:
                    values = await asyncio.wait_for(provider.search(query), timeout=(
                        provider.config["timeout"] if isinstance(provider, UserJsonProvider) else PROVIDER_TIMEOUT_SECONDS
                    ))
                    return values, {"id": provider.provider_id, "name": provider.name, "status": "ok", "count": len(values)}
                except asyncio.CancelledError:
                    raise
                except asyncio.TimeoutError:
                    return [], {"id": provider.provider_id, "name": provider.name, "status": "timeout", "count": 0, "error": "Search timed out"}
                except Exception as error:
                    detail = str(error).strip() or error.__class__.__name__
                    return [], {"id": provider.provider_id, "name": provider.name, "status": "error", "count": 0, "error": detail[:240]}

            outcomes = await asyncio.gather(*(run_provider(provider) for provider in providers))
            raw_results = [result for values, _ in outcomes for result in values]
            statuses = [status for _, status in outcomes]
            results = _deduplicate(raw_results, query)
            for result in results:
                if result.get("quality"):
                    result["quality_rank"] = _QUALITY_RANK.get(str(result["quality"]).casefold(), 0)
                else:
                    result["quality_rank"] = 0

            if check_availability and results:
                candidates = results[:MAX_SEARCH_RESULTS]
                probes = await probe_urls(
                    [result["url"] for result in candidates],
                    [result.get("headers") or {} for result in candidates],
                )
                by_url = {probe["url"]: probe for probe in probes}
                for result in results[:MAX_SEARCH_RESULTS]:
                    probe = by_url.get(result["url"], {})
                    result["available"] = bool(probe.get("live"))
                    result["status"] = probe.get("status")
                    result["content_type"] = probe.get("content_type")
                    if not result.get("quality"):
                        content_type = str(result.get("content_type") or "").lower()
                        if "mpegurl" in content_type or "vnd.apple" in content_type:
                            result["quality"] = "adaptive"
            results.sort(key=lambda result: (
                bool(result.get("available")),
                result["score"],
                result.get("quality_rank", 0),
                bool(result.get("year")),
                result["title"].casefold(),
            ), reverse=True)
            results = results[:MAX_SEARCH_RESULTS]
            for result in results:
                result.pop("quality_rank", None)
            response = {"query": query, "results": results, "providers": statuses}
            self._cache[key] = (now + SEARCH_CACHE_SECONDS, copy.deepcopy(response))
            self._cache.move_to_end(key)
            while len(self._cache) > MAX_CACHE_ENTRIES:
                self._cache.popitem(last=False)
            return response

    def clear_cache(self) -> None:
        self._cache.clear()


search_engine = SearchEngine()

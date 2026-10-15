"""TMDB (The Movie Database) metadata enrichment for the Lumen library.

The functions in the first half of this module are pure: guessing a title from a
playlist entry name, normalizing titles, deciding whether a search hit is really
the title we asked about, choosing how to authenticate, and mapping a TMDB details
payload onto our channel columns. They are deliberately free of I/O so they can be
unit-tested without a network call or an API key.

`TmdbClient` wraps the HTTP calls and accepts an injected `httpx.AsyncClient`, so
tests can hand it an `httpx.MockTransport` instead of the real network.
"""

from __future__ import annotations

import asyncio
import json
import re
import time
from typing import Any, Dict, Iterable, List, Optional, Tuple

import httpx

from .extract import MAX_API_RESPONSE_BYTES, read_capped_body

API_BASE = "https://api.themoviedb.org/3"
IMAGE_BASE = "https://image.tmdb.org/t/p/"
POSTER_SIZE = "w342"
BACKDROP_SIZE = "w780"
LANGUAGE = "en-US"
REQUEST_DELAY_SECONDS = 0.12
DEFAULT_LIMIT = 40

# Resolution and quality markers that fill IPTV channel names but never appear in
# a real film title, so they have to go before we ask TMDB about the name.
_QUALITY_TOKENS = (
    "480p", "504p", "540p", "576p", "720p", "1080p", "1280p", "1440p", "2160p",
    "4k", "8k", "uhd", "fhd", "hd", "sd", "hq", "h264", "hevc", "mp4",
)

_BRACKET_GROUPS = re.compile(r"\([^)]*\)|\[[^\]]*\]")
_QUALITY_RE = re.compile(r"\b(?:" + "|".join(_QUALITY_TOKENS) + r")\b", re.IGNORECASE)
_WHITESPACE = re.compile(r"\s+")
_TRAILING_JUNK = re.compile(r"[\s\-|:]+$")
_NON_TITLE_CHARS = re.compile(r"[^a-z0-9 ]+")

# A v4 read-access token is a JWT; a v3 API key is an opaque string. Both are
# accepted by the API, they just travel differently.
_BEARER_PREFIX = "eyJ"

TMDB_COLUMNS: Tuple[Tuple[str, str], ...] = (
    ("tmdb_id", "INTEGER"),
    ("tmdb_type", "TEXT"),
    ("poster_path", "TEXT"),
    ("backdrop_path", "TEXT"),
    ("overview", "TEXT"),
    ("release_year", "INTEGER"),
    ("rating", "REAL"),
    ("genre_names", "TEXT"),
    ("enriched_at", "INTEGER"),
)


class TmdbAuthError(RuntimeError):
    """Raised when TMDB rejects the key, so the caller aborts the whole run."""


def clean_title(name: Optional[str]) -> str:
    """Turn a playlist entry name into something TMDB might actually know.

    ``"BFM Business (1080p)"`` -> ``"BFM Business"``
    ``"Online TV Nusantara (720p) [Not 24/7]"`` -> ``"Online TV Nusantara"``
    ``"Big Buck Bunny (Open Movie HLS)"`` -> ``"Big Buck Bunny"``

    Returns an empty string when nothing title-like survives.
    """
    if not name:
        return ""
    text = _BRACKET_GROUPS.sub(" ", name)
    text = _QUALITY_RE.sub(" ", text)
    text = _WHITESPACE.sub(" ", text).strip()
    text = _TRAILING_JUNK.sub("", text)
    return text.strip()


def normalize_title(value: Optional[str]) -> str:
    """Lowercase, strip punctuation, collapse spaces, drop a leading article."""
    if not value:
        return ""
    text = _NON_TITLE_CHARS.sub(" ", value.strip().lower())
    text = _WHITESPACE.sub(" ", text).strip()
    if text.startswith("the "):
        text = text[4:].strip()
    return text


def titles_match(guess: str, candidate: Optional[str]) -> bool:
    """Decide whether a TMDB search hit is really the title we asked about.

    Exact normalized equality always wins. Otherwise a top hit is accepted only
    when the query is at least 4 characters and the hit actually contains it.
    Most IPTV channel names are not films, so rejecting junk matters more than
    forcing a match.
    """
    normalized_guess = normalize_title(guess)
    normalized_candidate = normalize_title(candidate)
    if not normalized_guess or not normalized_candidate:
        return False
    if normalized_guess == normalized_candidate:
        return True
    return len(normalized_guess) >= 4 and normalized_guess in normalized_candidate


def uses_bearer_token(api_key: str) -> bool:
    """True for a JWT read-access token, false for a v3 API key."""
    return bool(api_key) and api_key.startswith(_BEARER_PREFIX)


def auth_request_kwargs(api_key: str) -> Dict[str, Any]:
    """Return the headers and query params that authenticate a TMDB v3 request."""
    if uses_bearer_token(api_key):
        return {"headers": {"Authorization": f"Bearer {api_key}"}, "params": {}}
    return {"headers": {}, "params": {"api_key": api_key}}


def first_year(raw: Optional[str]) -> Optional[int]:
    """Parse the leading year out of a TMDB date string, or None when absent."""
    if not raw or len(raw) < 4:
        return None
    head = raw[:4]
    return int(head) if head.isdigit() else None


def build_row_updates(media_type: str, details: Dict[str, Any]) -> Dict[str, Any]:
    """Map a TMDB details payload onto our channel columns."""
    genres = [g.get("name") for g in (details.get("genres") or []) if g.get("name")]
    vote = details.get("vote_average")
    date_field = "release_date" if media_type == "movie" else "first_air_date"
    return {
        "tmdb_id": details.get("id"),
        "tmdb_type": media_type,
        "poster_path": details.get("poster_path"),
        "backdrop_path": details.get("backdrop_path"),
        "overview": details.get("overview") or None,
        "release_year": first_year(details.get(date_field)),
        "rating": round(float(vote), 1) if isinstance(vote, (int, float)) else None,
        "genre_names": ";".join(genres) if genres else None,
    }


class TmdbClient:
    """Thin TMDB client. Pass `client` to inject a test transport."""

    def __init__(
        self,
        api_key: str,
        *,
        client: Optional[httpx.AsyncClient] = None,
        delay: float = REQUEST_DELAY_SECONDS,
    ) -> None:
        self.api_key = api_key
        self._client = client
        self._owns_client = client is None
        self.delay = delay
        self.requests = 0

    async def _get(self, path: str, **params: Any) -> Dict[str, Any]:
        if self._client is None:
            # Never follow redirects: the API key travels with every request.
            self._client = httpx.AsyncClient(timeout=20.0, follow_redirects=False)
        auth = auth_request_kwargs(self.api_key)
        merged = {**auth["params"], "language": LANGUAGE, **params}
        self.requests += 1
        # Stream so the size cap applies before the body is buffered.
        async with self._client.stream(
            "GET", f"{API_BASE}{path}", params=merged, headers=auth["headers"]
        ) as response:
            if response.status_code == 401:
                raise TmdbAuthError("TMDB rejected the API key (HTTP 401)")
            response.raise_for_status()
            body = await read_capped_body(response, MAX_API_RESPONSE_BYTES, "TMDB response")
        return json.loads(body)

    async def search(self, title: str) -> Optional[Tuple[str, Dict[str, Any]]]:
        """Search movies first, then TV. Returns (media_type, hit) or None."""
        for media_type, path in (("movie", "/search/movie"), ("tv", "/search/tv")):
            payload = await self._get(path, query=title, include_adult="false")
            results = payload.get("results") or []
            if not results:
                continue
            top = results[0]
            if titles_match(title, top.get("title") or top.get("name")):
                return media_type, top
        return None

    async def details(self, media_type: str, tmdb_id: Any) -> Dict[str, Any]:
        return await self._get(f"/{media_type}/{tmdb_id}")

    async def aclose(self) -> None:
        if self._client is not None and self._owns_client:
            await self._client.aclose()


async def enrich_channels(
    api_key: str,
    candidates: Iterable[Tuple[int, str]],
    store,
    *,
    client: Optional[httpx.AsyncClient] = None,
    delay: float = REQUEST_DELAY_SECONDS,
    now: Optional[int] = None,
) -> Dict[str, int]:
    """Enrich `candidates` (iterable of ``(channel_id, name)``) one at a time.

    `store(channel_id, updates)` persists a single row and is called for EVERY
    candidate, so a row that found no match is still stamped as attempted and a
    re-run will not ask TMDB about it again.

    One request is in flight at a time with a short delay between them. A failure
    on a single candidate counts as skipped and does not abort the run, but a
    rejected key (HTTP 401) does.
    """
    stamp = int(time.time() if now is None else now)
    api = TmdbClient(api_key, client=client, delay=delay)
    stats = {"enriched": 0, "skipped": 0, "requests": 0}

    try:
        for channel_id, name in candidates:
            guess = clean_title(name)
            updates: Dict[str, Any] = {}

            if len(guess) < 2:
                stats["skipped"] += 1
            else:
                try:
                    found = await api.search(guess)
                    if found is None:
                        stats["skipped"] += 1
                    else:
                        media_type, hit = found
                        details = await api.details(media_type, hit.get("id"))
                        updates = build_row_updates(media_type, details)
                        stats["enriched"] += 1
                except TmdbAuthError:
                    raise
                except Exception:
                    # A single bad response must not cost us the whole batch.
                    stats["skipped"] += 1

            updates["enriched_at"] = stamp
            store(channel_id, updates)

            if delay:
                await asyncio.sleep(delay)
    finally:
        await api.aclose()

    stats["requests"] = api.requests
    # `matched` is the in-run match count, kept because the shared response shape
    # exposes it alongside `enriched`.
    stats["matched"] = stats["enriched"]
    return stats

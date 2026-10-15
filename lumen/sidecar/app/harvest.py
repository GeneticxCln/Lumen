from __future__ import annotations

import asyncio
import json
import re
from typing import List
from urllib.parse import urljoin, urlparse

import httpx
from bs4 import BeautifulSoup

from .extract import assert_public_request, ensure_public_http_url

MAX_HARVEST_BYTES = 5 * 1024 * 1024
HARVEST_TIMEOUT_SECONDS = 20.0

STREAM_URL_RE = re.compile(
    r"""https?://[^\s"'<>\\]+?\.(?:m3u8|m3u)(?:\?[^\s"'<>\\]*)?""",
    re.IGNORECASE,
)

RELATIVE_M3U_RE = re.compile(
    r"""\.(?:m3u8|m3u)(?:\?|$)""",
    re.IGNORECASE,
)


def _collect_from_json_ld(data: object, base_url: str, found: list[str]) -> None:
    if isinstance(data, dict):
        for k, v in data.items():
            if isinstance(v, str) and (
                k in {"contentUrl", "embedUrl", "url"} or RELATIVE_M3U_RE.search(v)
            ):
                resolved = urljoin(base_url, v.strip())
                if RELATIVE_M3U_RE.search(resolved):
                    found.append(resolved)
            else:
                _collect_from_json_ld(v, base_url, found)
    elif isinstance(data, list):
        for item in data:
            _collect_from_json_ld(item, base_url, found)


async def _fetch_page(client: httpx.AsyncClient, url: str) -> tuple[str, str]:
    async with client.stream("GET", url) as resp:
        resp.raise_for_status()
        chunks: list[bytes] = []
        total = 0
        async for chunk in resp.aiter_bytes():
            total += len(chunk)
            if total > MAX_HARVEST_BYTES:
                raise ValueError("Page is too large to scan for streams")
            chunks.append(chunk)
        text = b"".join(chunks).decode(resp.encoding or "utf-8", errors="replace")
        return str(resp.url), text


async def harvest_streams(url: str) -> List[str]:
    validated = ensure_public_http_url(url)

    # If the user directly pasted an .m3u/.m3u8 URL, include it immediately
    found: list[str] = []
    if RELATIVE_M3U_RE.search(validated):
        found.append(validated)

    headers = {
        "User-Agent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Lumen/0.1",
        "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    }

    async with httpx.AsyncClient(
        timeout=15.0,
        follow_redirects=True,
        headers=headers,
        event_hooks={"request": [assert_public_request]},
    ) as client:
        base_url, text = await asyncio.wait_for(
            _fetch_page(client, validated), timeout=HARVEST_TIMEOUT_SECONDS
        )

    # 1. Parse HTML tags (<a href>, <video src>, <source src>, data-src, etc.)
    soup = BeautifulSoup(text, "lxml")
    for tag in soup.find_all(["a", "video", "source", "iframe", "link"]):
        for attr in ("href", "src", "data-src", "data-url", "data-stream"):
            val = tag.get(attr)
            if isinstance(val, str) and val.strip():
                candidate = urljoin(base_url, val.strip())
                parsed = urlparse(candidate)
                if parsed.scheme in {"http", "https"} and RELATIVE_M3U_RE.search(candidate):
                    found.append(candidate)

    # 2. Parse JSON-LD blocks
    for script in soup.find_all("script", attrs={"type": re.compile(r"ld\+json", re.I)}):
        raw = script.string or script.get_text()
        if raw:
            try:
                parsed_json = json.loads(raw)
                _collect_from_json_ld(parsed_json, base_url, found)
            except Exception:
                pass

    # 3. Regex scan over raw HTML/inline scripts for .m3u / .m3u8 URLs
    for match in STREAM_URL_RE.findall(text):
        cleaned = match.rstrip(".,;)]}'\"")
        found.append(cleaned)

    # Deduplicate while preserving order
    seen: set[str] = set()
    unique: list[str] = []
    for item in found:
        if item not in seen:
            seen.add(item)
            unique.append(item)

    return unique

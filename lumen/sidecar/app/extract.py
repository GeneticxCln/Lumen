from __future__ import annotations

import asyncio
from typing import Dict
from urllib.parse import urlparse

import yt_dlp


ALLOWED_SCHEMES = {"http", "https"}


def validate_http_url(url: str) -> str:
    parsed = urlparse(url.strip())
    if parsed.scheme.lower() not in ALLOWED_SCHEMES or not parsed.netloc:
        raise ValueError("URL must use http:// or https://")
    return url.strip()


def _extract_sync(url: str) -> dict:
    validated = validate_http_url(url)
    ydl_opts = {
        "quiet": True,
        "no_warnings": True,
        "noplaylist": True,
        "format": "best/bestvideo+bestaudio",
        "socket_timeout": 15,
    }
    with yt_dlp.YoutubeDL(ydl_opts) as ydl:
        info = ydl.extract_info(validated, download=False)
        if not info:
            raise RuntimeError("No stream information extracted")

        # Handle playlist / multi-video results by picking first entry
        if "entries" in info and info["entries"]:
            entries = [e for e in info["entries"] if e]
            if not entries:
                raise RuntimeError("Playlist had no playable entries")
            info = entries[0]

        stream_url = info.get("url")
        if not stream_url and "requested_formats" in info and info["requested_formats"]:
            stream_url = info["requested_formats"][0].get("url")
        if not stream_url and "formats" in info and info["formats"]:
            # Pick last format with a direct URL
            with_urls = [f for f in info["formats"] if f.get("url")]
            if with_urls:
                stream_url = with_urls[-1]["url"]

        if not stream_url:
            raise RuntimeError("Could not resolve a direct playable stream URL")

        raw_headers = info.get("http_headers") or {}
        headers: Dict[str, str] = {str(k): str(v) for k, v in raw_headers.items() if v is not None}

        title = info.get("title") or validated
        return {
            "title": str(title),
            "url": str(stream_url),
            "headers": headers,
        }


async def extract_stream(url: str) -> dict:
    return await asyncio.to_thread(_extract_sync, url)

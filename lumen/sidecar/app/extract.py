from __future__ import annotations

import asyncio
import ipaddress
import socket
from typing import Dict
from urllib.parse import urlparse

import httpx
import yt_dlp


ALLOWED_SCHEMES = {"http", "https"}


def validate_http_url(url: str) -> str:
    parsed = urlparse(url.strip())
    if parsed.scheme.lower() not in ALLOWED_SCHEMES or not parsed.netloc:
        raise ValueError("URL must use http:// or https://")
    return url.strip()


def _is_public_address(address: str) -> bool:
    ip = ipaddress.ip_address(address.split("%", 1)[0])
    return not (
        ip.is_private
        or ip.is_loopback
        or ip.is_link_local
        or ip.is_multicast
        or ip.is_reserved
        or ip.is_unspecified
    )


def ensure_public_http_url(url: str) -> str:
    """Validate an http(s) URL and reject hosts that resolve to local or private addresses."""
    validated = validate_http_url(url)
    host = urlparse(validated).hostname
    if not host:
        raise ValueError("URL must include a host")
    try:
        infos = socket.getaddrinfo(host, None, type=socket.SOCK_STREAM)
    except socket.gaierror as error:
        raise ValueError(f"Could not resolve host {host}") from error
    for info in infos:
        if not _is_public_address(str(info[4][0])):
            raise ValueError("URL resolves to a private or local address")
    return validated


async def assert_public_request(request: httpx.Request) -> None:
    """httpx request hook: runs for the initial request and every redirect hop."""
    await asyncio.to_thread(ensure_public_http_url, str(request.url))


# Largest JSON body read from a metadata or search API. A bigger response is refused, never buffered.
MAX_API_RESPONSE_BYTES = 8 * 1024 * 1024


async def read_capped_body(response: httpx.Response, limit: int, label: str) -> bytes:
    """Read a streamed body, stopping as soon as its size passes ``limit``.

    The response must come from ``client.stream``; a plain ``client.get`` has already
    buffered the whole body before this check can run.
    """
    declared = response.headers.get("content-length")
    if declared is not None and declared.isdigit() and int(declared) > limit:
        raise ValueError(f"{label} is {declared} bytes, over the {limit} byte limit")
    buffer = bytearray()
    async for chunk in response.aiter_bytes():
        if len(buffer) + len(chunk) > limit:
            raise ValueError(f"{label} is over the {limit} byte limit")
        buffer.extend(chunk)
    return bytes(buffer)


def _extract_sync(url: str) -> dict:
    validated = ensure_public_http_url(url)
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

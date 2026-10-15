from __future__ import annotations

import asyncio
from typing import List, Optional
from urllib.parse import urljoin, urlparse

import httpx

from .extract import assert_public_request

MAX_CONCURRENT_PROBES = 8
PROBE_TIMEOUT_SECONDS = 5.0
MAX_SAME_HOST_REDIRECTS = 5


async def _probe_single(url: str, semaphore: asyncio.Semaphore, extra_headers: Optional[dict[str, str]] = None) -> dict:
    parsed = urlparse(url.strip())
    if parsed.scheme.lower() not in {"http", "https"} or not parsed.netloc or parsed.username or parsed.password:
        return {
            "url": url,
            "live": False,
            "status": None,
            "content_type": None,
        }

    origin_host = parsed.hostname
    current_url = url.strip()
    origin_port = parsed.port or (443 if parsed.scheme.lower() == "https" else 80)

    async with semaphore:
        timeout = httpx.Timeout(PROBE_TIMEOUT_SECONDS)
        headers = {
            "User-Agent": "Mozilla/5.0 (X11; Linux x86_64) Lumen/0.1",
            **(extra_headers or {}),
        }
        try:
            async with httpx.AsyncClient(
                timeout=timeout,
                follow_redirects=False,
                headers=headers,
                event_hooks={"request": [assert_public_request]},
            ) as client:
                for _ in range(MAX_SAME_HOST_REDIRECTS + 1):
                    # Try HEAD first
                    resp = await client.head(current_url)

                    # Some IPTV servers reject HEAD with 405/403/400; fall back to a streamed GET
                    if resp.status_code in {400, 403, 404, 405, 501}:
                        async with client.stream("GET", current_url) as stream_resp:
                            status = stream_resp.status_code
                            ctype = stream_resp.headers.get("content-type")
                            loc = stream_resp.headers.get("location")
                            if status in {301, 302, 303, 307, 308} and loc:
                                next_url = urljoin(current_url, loc)
                                next_parsed = urlparse(next_url)
                                if next_parsed.hostname != origin_host or (next_parsed.port or (443 if next_parsed.scheme.lower() == "https" else 80)) != origin_port:
                                    return {
                                        "url": url,
                                        "live": False,
                                        "status": status,
                                        "content_type": ctype,
                                    }
                                current_url = next_url
                                continue
                            live = 200 <= status < 400
                            return {
                                "url": url,
                                "live": live,
                                "status": status,
                                "content_type": ctype,
                            }

                    status = resp.status_code
                    ctype: Optional[str] = resp.headers.get("content-type")
                    loc = resp.headers.get("location")

                    if status in {301, 302, 303, 307, 308} and loc:
                        next_url = urljoin(current_url, loc)
                        # Enforce same-host redirect policy per PLAN.md §4.2 / §11
                        next_parsed = urlparse(next_url)
                        if next_parsed.hostname != origin_host or (next_parsed.port or (443 if next_parsed.scheme.lower() == "https" else 80)) != origin_port:
                            return {
                                "url": url,
                                "live": False,
                                "status": status,
                                "content_type": ctype,
                            }
                        current_url = next_url
                        continue

                    live = 200 <= status < 400
                    return {
                        "url": url,
                        "live": live,
                        "status": status,
                        "content_type": ctype,
                    }

                return {
                    "url": url,
                    "live": False,
                    "status": None,
                    "content_type": None,
                }
        except Exception:
            return {
                "url": url,
                "live": False,
                "status": None,
                "content_type": None,
            }


async def probe_urls(urls: List[str], headers: Optional[List[dict[str, str]]] = None) -> List[dict]:
    semaphore = asyncio.Semaphore(MAX_CONCURRENT_PROBES)
    tasks = [
        _probe_single(url, semaphore, headers[index] if headers and index < len(headers) else None)
        for index, url in enumerate(urls)
    ]
    return await asyncio.gather(*tasks)

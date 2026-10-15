from __future__ import annotations

import asyncio
from typing import List, Optional
from urllib.parse import urljoin, urlparse

import httpx

MAX_CONCURRENT_PROBES = 8
PROBE_TIMEOUT_SECONDS = 5.0
MAX_SAME_HOST_REDIRECTS = 5


async def _probe_single(url: str, semaphore: asyncio.Semaphore) -> dict:
    parsed = urlparse(url.strip())
    if parsed.scheme.lower() not in {"http", "https"} or not parsed.netloc:
        return {
            "url": url,
            "live": False,
            "status": None,
            "content_type": None,
        }

    origin_host = parsed.hostname
    current_url = url.strip()

    async with semaphore:
        timeout = httpx.Timeout(PROBE_TIMEOUT_SECONDS)
        headers = {
            "User-Agent": "Mozilla/5.0 (X11; Linux x86_64) Lumen/0.1",
        }
        try:
            async with httpx.AsyncClient(timeout=timeout, follow_redirects=False, headers=headers) as client:
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
                                if urlparse(next_url).hostname != origin_host:
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
                        if urlparse(next_url).hostname != origin_host:
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


async def probe_urls(urls: List[str]) -> List[dict]:
    semaphore = asyncio.Semaphore(MAX_CONCURRENT_PROBES)
    tasks = [_probe_single(u, semaphore) for u in urls]
    return await asyncio.gather(*tasks)

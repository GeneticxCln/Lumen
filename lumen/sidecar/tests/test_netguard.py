from __future__ import annotations

import asyncio
import http.server
import sys
import threading
import unittest
from pathlib import Path

import httpx

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from app.extract import assert_public_request, ensure_public_http_url  # noqa: E402
from app.probe import _probe_single  # noqa: E402


class PublicUrlGuardTests(unittest.TestCase):
    def test_rejects_loopback_private_and_metadata_addresses(self) -> None:
        for url in (
            "http://127.0.0.1:43127/",
            "http://10.0.0.5/playlist.m3u8",
            "http://192.168.1.10/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/",
        ):
            with self.subTest(url=url):
                with self.assertRaises(ValueError):
                    ensure_public_http_url(url)

    def test_rejects_non_http_schemes(self) -> None:
        with self.assertRaises(ValueError):
            ensure_public_http_url("file:///etc/passwd")

    def test_accepts_public_address_literal(self) -> None:
        self.assertEqual(
            ensure_public_http_url("https://93.184.216.34/video.m3u8"),
            "https://93.184.216.34/video.m3u8",
        )


class RedirectHopGuardTests(unittest.IsolatedAsyncioTestCase):
    async def test_redirect_into_private_address_is_blocked(self) -> None:
        def handler(request: httpx.Request) -> httpx.Response:
            if request.url.host == "93.184.216.34":
                return httpx.Response(302, headers={"Location": "http://127.0.0.1:43127/secret"})
            return httpx.Response(200, text="should not be reached")

        async with httpx.AsyncClient(
            transport=httpx.MockTransport(handler),
            follow_redirects=True,
            event_hooks={"request": [assert_public_request]},
        ) as client:
            with self.assertRaises(ValueError):
                await client.get("https://93.184.216.34/start")


class ProbeLoopbackGuardTests(unittest.IsolatedAsyncioTestCase):
    async def test_probe_never_reaches_loopback_server(self) -> None:
        hits: list[str] = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def _record(self) -> None:
                hits.append(self.path)
                self.send_response(200)
                self.end_headers()

            do_GET = _record
            do_HEAD = _record

            def log_message(self, *args: object) -> None:
                pass

        server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            port = server.server_address[1]
            result = await _probe_single(
                f"http://127.0.0.1:{port}/stream.m3u8",
                asyncio.Semaphore(1),
            )
        finally:
            server.shutdown()
            server.server_close()

        self.assertFalse(result["live"])
        self.assertIsNone(result["status"])
        self.assertEqual(hits, [])


if __name__ == "__main__":
    unittest.main()

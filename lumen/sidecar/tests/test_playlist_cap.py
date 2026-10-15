from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path

# The bridge creates its database at import time, so point it at a scratch directory first.
os.environ["LUMEN_DATA_DIR"] = tempfile.mkdtemp(prefix="lumen-cap-test-")
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import httpx  # noqa: E402

from app import bridge  # noqa: E402
from app.extract import MAX_API_RESPONSE_BYTES, read_capped_body  # noqa: E402
from app.search_engine import ProviderManager  # noqa: E402
from app.tmdb import TmdbClient  # noqa: E402


class ChunkedBody(httpx.AsyncByteStream):
    """A finite body with no declared length, so the streaming check is what catches it."""

    def __init__(self, chunks: list[bytes]) -> None:
        self.chunks = chunks

    async def __aiter__(self):
        for chunk in self.chunks:
            yield chunk


class EndlessBody(httpx.AsyncByteStream):
    """An unbounded body that counts how many chunks have been pulled from it."""

    def __init__(self) -> None:
        self.reads = 0

    async def __aiter__(self):
        while True:
            self.reads += 1
            yield b"x" * 1024


async def read_with(response: httpx.Response, limit: int) -> str:
    def handler(request: httpx.Request) -> httpx.Response:
        return response

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
        async with client.stream("GET", "https://example.test/list.m3u") as resp:
            return await bridge.read_capped_text(resp, limit)


class PlaylistCapTests(unittest.IsolatedAsyncioTestCase):
    async def test_body_exactly_at_the_limit_is_read_in_full(self) -> None:
        text = await read_with(httpx.Response(200, content=b"a" * 100), 100)
        self.assertEqual(text, "a" * 100)

    async def test_body_one_byte_over_the_limit_is_refused(self) -> None:
        with self.assertRaises(ValueError):
            await read_with(httpx.Response(200, stream=ChunkedBody([b"a" * 60, b"a" * 41])), 100)

    async def test_declared_length_over_the_limit_is_refused_before_reading(self) -> None:
        body = EndlessBody()
        response = httpx.Response(200, stream=body, headers={"content-length": "5000"})
        with self.assertRaises(ValueError):
            await read_with(response, 100)
        self.assertEqual(body.reads, 0, "no body bytes should be read when the declared length is too large")

    async def test_endless_body_is_cut_off_at_the_limit(self) -> None:
        body = EndlessBody()
        with self.assertRaises(ValueError):
            await read_with(httpx.Response(200, stream=body), 4096)
        # Four 1 KiB chunks fit in 4 KiB; the fifth pushes the total over, so reading stops there.
        self.assertEqual(body.reads, 5)

    async def test_utf8_playlist_text_is_decoded(self) -> None:
        text = await read_with(httpx.Response(200, content="#EXTM3U\n#EXTINF:-1,Café\n".encode("utf-8")), 1024)
        self.assertIn("Café", text)


class ApiResponseCapTests(unittest.IsolatedAsyncioTestCase):
    async def test_declared_length_over_the_limit_is_refused_with_its_label(self) -> None:
        body = EndlessBody()
        response = httpx.Response(200, stream=body, headers={"content-length": "5000"})
        async with httpx.AsyncClient(transport=httpx.MockTransport(lambda request: response)) as client:
            async with client.stream("GET", "https://example.test/api") as resp:
                with self.assertRaisesRegex(ValueError, "Widget response is 5000 bytes"):
                    await read_capped_body(resp, 100, "Widget response")
        self.assertEqual(body.reads, 0)

    async def test_tmdb_response_stream_is_cut_off_at_the_api_limit(self) -> None:
        body = EndlessBody()
        client = httpx.AsyncClient(transport=httpx.MockTransport(lambda request: httpx.Response(200, stream=body)))
        tmdb = TmdbClient("v3-key", client=client, delay=0)
        with self.assertRaisesRegex(ValueError, "TMDB response is over the"):
            await tmdb._get("/search/movie", query="example")
        await client.aclose()
        # The reader stops once the running total passes the limit, so it never pulls the whole endless body.
        self.assertLessEqual(body.reads * 1024, MAX_API_RESPONSE_BYTES + 1024)

    async def test_tmdb_json_within_the_limit_still_parses(self) -> None:
        client = httpx.AsyncClient(transport=httpx.MockTransport(lambda request: httpx.Response(200, json={"results": [{"id": 7}]})))
        tmdb = TmdbClient("v3-key", client=client, delay=0)
        payload = await tmdb._get("/search/movie", query="example")
        await client.aclose()
        self.assertEqual(payload, {"results": [{"id": 7}]})

    async def test_search_provider_response_stream_is_cut_off_at_the_api_limit(self) -> None:
        body = EndlessBody()
        client = httpx.AsyncClient(transport=httpx.MockTransport(lambda request: httpx.Response(200, stream=body)))
        provider = ProviderManager(client).build([], [{
            "id": "remote", "name": "Remote", "endpoint": "https://provider.test/search",
        }])[1]
        with self.assertRaisesRegex(ValueError, "Remote response is over the"):
            await provider.search("example")
        await client.aclose()
        self.assertLessEqual(body.reads * 1024, MAX_API_RESPONSE_BYTES + 1024)


if __name__ == "__main__":
    unittest.main()

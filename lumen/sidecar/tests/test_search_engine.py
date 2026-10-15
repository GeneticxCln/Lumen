from __future__ import annotations

import asyncio
import unittest
from unittest.mock import AsyncMock, patch

import httpx

from app.search_engine import LibraryProvider, ProviderManager, SearchEngine, _deduplicate, _validate_provider


class NormalizationTests(unittest.TestCase):
    def test_provider_config_requires_safe_http_endpoint_and_validates_limits(self):
        with self.assertRaises(ValueError):
            _validate_provider({"id": "bad", "name": "Bad", "endpoint": "file:///etc/passwd"})
        with self.assertRaises(ValueError):
            _validate_provider({"id": "bad", "name": "Bad", "endpoint": "https://user:pass@example.com/search"})
        value = _validate_provider({"id": "json", "name": "JSON", "endpoint": "https://example.com/search", "timeout": 100})
        self.assertEqual(value["timeout"], 15.0)

    def test_library_provider_matches_title_aliases_and_preserves_metadata(self):
        provider = LibraryProvider([{
            "id": 5, "playlist_id": 3, "name": "The Example Movie (1080p)",
            "url": "https://cdn.example/movie.m3u8", "group_title": "Films; Drama",
            "release_year": 2024, "tmdb_type": "movie", "extra_headers": '{"Referer":"https://example.com"}',
        }])
        results = asyncio.run(provider.search("drama example"))
        self.assertEqual(len(results), 1)
        self.assertEqual(results[0]["channel_id"], 5)
        self.assertEqual(results[0]["media_type"], "movie")
        self.assertEqual(results[0]["headers"]["Referer"], "https://example.com")

    def test_title_deduplication_merges_provider_alias_and_metadata(self):
        results = _deduplicate([
            {"id": "library:1", "title": "Example Film", "url": "https://a.test/movie.m3u8", "provider": "Library", "aliases": ["Example"], "year": 2020, "media_type": "movie", "quality": None, "language": None, "headers": {}, "channel_id": 1, "playlist_id": 2},
            {"id": "json:2", "title": "Example", "url": "https://b.test/movie.m3u8", "provider": "Remote", "aliases": ["Example Film"], "year": 2020, "media_type": "movie", "quality": "1080p", "language": "en", "headers": {"Referer": "https://source.test"}},
        ], "Example Film")
        self.assertEqual(len(results), 1)
        self.assertEqual(results[0]["providers"], ["Library", "Remote"])
        self.assertEqual(results[0]["channel_id"], 1)
        self.assertEqual(results[0]["quality"], "1080p")
        self.assertEqual(results[0]["language"], "en")

    def test_same_title_different_year_or_media_type_is_not_merged(self):
        rows = _deduplicate([
            {"title": "Example", "url": "https://a.test/1", "provider": "A", "aliases": [], "year": 2020, "media_type": "movie"},
            {"title": "Example", "url": "https://b.test/2", "provider": "B", "aliases": [], "year": 2021, "media_type": "movie"},
            {"title": "Example", "url": "https://c.test/3", "provider": "C", "aliases": [], "year": 2020, "media_type": "series"},
        ], "Example")
        self.assertEqual(len(rows), 3)


class ProviderTests(unittest.IsolatedAsyncioTestCase):
    async def test_provider_registry_rejects_duplicates_and_caps_count(self):
        transport = httpx.MockTransport(lambda request: httpx.Response(200, json={"results": []}))
        async with httpx.AsyncClient(transport=transport) as client:
            manager = ProviderManager(client)
            config = {"id": "remote", "name": "Remote", "endpoint": "https://example.test/search"}
            with self.assertRaisesRegex(ValueError, "Duplicate"):
                manager.build([], [config, config])
            with self.assertRaisesRegex(ValueError, "At most"):
                manager.build([], [dict(config, id=str(i)) for i in range(13)])

    async def test_user_json_provider_maps_configured_fields(self):
        async def handler(request: httpx.Request):
            self.assertEqual(request.url.params["q"], "Example")
            return httpx.Response(200, json={"data": [{"name": "Example Film", "stream": "https://cdn.example/movie.m3u8", "aka": ["Example"], "released": "2021", "resolution": "1080p", "lang": "en"}]})
        async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
            manager = ProviderManager(client)
            provider = manager.build([], [{
                "id": "remote", "name": "Remote", "endpoint": "https://provider.test/search",
                "results_path": "data", "title_field": "name", "url_field": "stream",
                "aliases_field": "aka", "year_field": "released", "quality_field": "resolution", "language_field": "lang",
            }])[1]
            found = await provider.search("Example")
        self.assertEqual(found[0]["year"], 2021)
        self.assertEqual(found[0]["quality"], "1080p")
        self.assertEqual(found[0]["language"], "en")

    async def test_orchestrator_isolates_provider_failure_times_out_and_caches(self):
        calls = {"ok": 0, "timeout": 0, "failure": 0}
        def handler(request: httpx.Request):
            provider = request.url.host
            calls["ok" if provider == "ok.test" else "failure"] += 1
            if provider == "bad.test":
                return httpx.Response(502, text="provider down")
            return httpx.Response(200, json={"results": [{"title": "Example Film", "url": "https://cdn.example/movie.m3u8", "year": 2022, "quality": "720p"}]})

        search_module = __import__("app.search_engine", fromlist=["UserJsonProvider"])
        original_search = search_module.UserJsonProvider.search
        original_validate = search_module._validate_provider

        async def selective_search(self, query):
            if self.provider_id == "slow":
                calls["timeout"] += 1
                await asyncio.sleep(0.05)
                return []
            return await original_search(self, query)

        def selective_validate(config):
            value = original_validate(config)
            if value["id"] == "slow":
                value["timeout"] = 0.01
            return value

        engine = SearchEngine()
        engine._client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
        config = lambda ident, host, timeout=2: {"id": ident, "name": ident, "endpoint": f"https://{host}/search", "timeout": timeout}
        with patch("app.search_engine.UserJsonProvider.search", selective_search), patch("app.search_engine._validate_provider", selective_validate):
            first = await engine.search("Example", [], [config("ok", "ok.test"), config("slow", "slow.test", 1), config("bad", "bad.test")], check_availability=False)
            self.assertEqual([item["title"] for item in first["results"]], ["Example Film"])
            self.assertTrue(any(provider["status"] == "error" for provider in first["providers"]))
            self.assertTrue(any(provider["status"] == "timeout" for provider in first["providers"]))
            second = await engine.search("Example", [], [config("ok", "ok.test"), config("slow", "slow.test", 1), config("bad", "bad.test")], check_availability=False)
        self.assertEqual(calls["ok"], 1, "cached results avoid repeating provider calls")
        self.assertEqual(calls["timeout"], 1, "cached results avoid repeating timeouts")
        self.assertEqual(second, first)
        await engine._client.aclose()

    async def test_orchestrator_evaluates_availability_and_hls_metadata(self):
        async def provider(request: httpx.Request):
            return httpx.Response(200, json={"results": [{"title": "Example Film", "url": "https://cdn.example/movie.m3u8"}]})
        engine = SearchEngine()
        engine._client = httpx.AsyncClient(transport=httpx.MockTransport(provider))
        with patch("app.search_engine.probe_urls", new=AsyncMock(return_value=[{"url": "https://cdn.example/movie.m3u8", "live": True, "status": 200, "content_type": "application/vnd.apple.mpegurl"}])):
            result = await engine.search("Example", [], [{"id": "remote", "name": "Remote", "endpoint": "https://provider.test/search"}])
        self.assertTrue(result["results"][0]["available"])
        self.assertEqual(result["results"][0]["quality"], "adaptive")
        self.assertEqual(result["results"][0]["content_type"], "application/vnd.apple.mpegurl")
        await engine._client.aclose()


if __name__ == "__main__":
    unittest.main()

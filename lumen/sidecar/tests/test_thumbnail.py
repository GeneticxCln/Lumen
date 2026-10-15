import asyncio
import importlib
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import httpx


class ThumbnailTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="lumen-thumbnail-tests-")
        self.data = Path(self.temp.name) / "data"
        self.data.mkdir()
        self.media = Path(self.temp.name) / "media"
        self.media.mkdir()
        env = patch.dict(os.environ, {
            "LUMEN_DATA_DIR": str(self.data),
            "LUMEN_STANDALONE": "1",
            "LUMEN_SEED_CATALOG": "0",
            "LUMEN_PREVIEW_PORT": "1420",
        })
        env.start()
        self.addCleanup(env.stop)
        # bridge.py initializes its DB on import; reset it for each test's temp directory.
        for name in ("app.bridge", "app.main"):
            sys.modules.pop(name, None)
        main = importlib.import_module("app.main")
        bridge = importlib.import_module("app.bridge")
        self.bridge = bridge
        self.main = main
        self.client = httpx.AsyncClient(
            transport=httpx.ASGITransport(app=main.app),
            base_url="http://127.0.0.1:43127",
            headers={
                "X-Lumen-Client": "local-preview",
                "Origin": "http://127.0.0.1:1420",
            },
        )
        self.addAsyncCleanup(self.client.aclose)
        video = self.media / "sample.mp4"
        generated = await asyncio.create_subprocess_exec(
            "ffmpeg", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
            "color=c=blue:s=640x360:d=1", "-frames:v", "1", "-y", str(video),
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
        )
        _, stderr = await generated.communicate()
        if generated.returncode != 0:
            self.fail(f"Could not create video fixture: {stderr.decode(errors='replace')}")
        self.video = video

    async def asyncTearDown(self):
        self.temp.cleanup()

    async def test_thumbnail_endpoint_generates_then_serves_cached_jpeg(self):
        path = f"/api/local-thumbnail?path={self.video}&client=local-preview"
        first = await self.client.get(path)
        self.assertEqual(first.status_code, 200, first.text)
        self.assertEqual(first.headers["content-type"], "image/jpeg")
        self.assertTrue(first.content.startswith(b"\xff\xd8"), "response contains an actual JPEG frame")
        self.assertLess(len(first.content), 2_000_000, "poster remains small enough for a card")
        cache_files = list((self.data / "thumbnails").glob("*.jpg"))
        self.assertEqual(len(cache_files), 1, "first request materializes a cached poster")
        before = cache_files[0].stat().st_mtime_ns

        second = await self.client.get(path)
        self.assertEqual(second.status_code, 200)
        self.assertEqual(second.content, first.content)
        self.assertEqual(cache_files[0].stat().st_mtime_ns, before, "cache hit does not re-decode or rewrite")

    async def test_non_video_or_missing_files_do_not_get_posters(self):
        text_file = self.media / "notes.txt"
        text_file.write_text("not a video")
        response = await self.client.get(f"/api/local-thumbnail?path={text_file}&client=local-preview")
        self.assertEqual(response.status_code, 404)
        missing = await self.client.get(f"/api/local-thumbnail?path={self.media / 'missing.mp4'}&client=local-preview")
        self.assertEqual(missing.status_code, 404)

    async def test_thumbnail_route_keeps_browser_bridge_protection(self):
        path = f"/api/local-thumbnail?path={self.video}&client=local-preview"
        bad_origin = await self.client.get(path, headers={"Origin": "https://attacker.invalid"})
        self.assertEqual(bad_origin.status_code, 403)


if __name__ == "__main__":
    unittest.main()

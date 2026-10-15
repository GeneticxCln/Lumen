import asyncio
import importlib
import os
from pathlib import Path
import shutil
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
            "LUMEN_LOCAL_MEDIA_ROOTS": str(self.media),
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
            "testsrc2=size=640x360:rate=2:duration=8", "-c:v", "mpeg4", "-y", str(video),
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

    async def test_filmstrip_endpoint_extracts_four_cached_frames(self):
        path = f"/api/local-filmstrip?path={self.video}&client=local-preview"
        first = await self.client.get(path)
        self.assertEqual(first.status_code, 200, first.text)
        self.assertEqual(first.headers["content-type"], "image/jpeg")
        self.assertTrue(first.content.startswith(b"\xff\xd8"), "response is a JPEG contact strip")
        self.assertLess(len(first.content), 2_000_000, "filmstrip remains small enough for a card")
        cache_files = list((self.data / "thumbnails").glob("*.jpg"))
        self.assertEqual(len(cache_files), 1, "first request materializes a filmstrip cache entry")
        before = cache_files[0].stat().st_mtime_ns

        image = self.media / "filmstrip.jpg"
        image.write_bytes(first.content)
        probe = await asyncio.create_subprocess_exec(
            "ffprobe", "-v", "error", "-show_entries", "stream=width,height", "-of", "csv=s=x:p=0", str(image),
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
        )
        stdout, stderr = await probe.communicate()
        self.assertEqual(probe.returncode, 0, stderr.decode(errors="replace"))
        self.assertEqual(stdout.decode().strip(), "960x136", "strip contains four 240x136 frames")

        second = await self.client.get(path)
        self.assertEqual(second.status_code, 200)
        self.assertEqual(second.content, first.content)
        self.assertEqual(cache_files[0].stat().st_mtime_ns, before, "cache hit does not regenerate the strip")

    async def test_non_video_or_missing_files_do_not_get_posters(self):
        text_file = self.media / "notes.txt"
        text_file.write_text("not a video")
        response = await self.client.get(f"/api/local-thumbnail?path={text_file}&client=local-preview")
        self.assertEqual(response.status_code, 404)
        missing = await self.client.get(f"/api/local-thumbnail?path={self.media / 'missing.mp4'}&client=local-preview")
        self.assertEqual(missing.status_code, 404)

    async def test_poster_frame_setting_validates_and_regenerates_posters(self):
        path = f"/api/local-thumbnail?path={self.video}&client=local-preview"
        self.assertEqual((await self.client.get(path)).status_code, 200)
        first_names = {file.name for file in (self.data / "thumbnails").glob("*.jpg")}
        self.assertEqual(len(first_names), 1)

        rejected = await self.client.post("/api/invoke/set_setting_cmd", json={"key": "poster_frame_seconds", "value": "7"})
        self.assertEqual(rejected.status_code, 400, "only offered frame times are stored")
        self.assertEqual(len(list((self.data / "thumbnails").glob("*.jpg"))), 1, "rejected changes keep the cache")

        saved = await self.client.post("/api/invoke/set_setting_cmd", json={"key": "poster_frame_seconds", "value": "0"})
        self.assertEqual(saved.status_code, 200, saved.text)
        self.assertFalse((self.data / "thumbnails").exists(), "changing the frame time clears stale posters")
        settings = (await self.client.post("/api/invoke/get_settings", json={})).json()
        self.assertEqual(settings["poster_frame_seconds"], "0")

        regenerated = await self.client.get(path)
        self.assertEqual(regenerated.status_code, 200, regenerated.text)
        new_names = {file.name for file in (self.data / "thumbnails").glob("*.jpg")}
        self.assertEqual(len(new_names), 1, "the poster regenerates with the new frame time")
        self.assertNotEqual(new_names, first_names, "the cache key includes the frame time")

        same = await self.client.post("/api/invoke/set_setting_cmd", json={"key": "poster_frame_seconds", "value": "0"})
        self.assertEqual(same.status_code, 200)
        self.assertEqual(len(list((self.data / "thumbnails").glob("*.jpg"))), 1, "saving an unchanged frame time keeps the cache")

    async def test_poster_candidates_render_offered_frames_and_mark_missing_ones(self):
        response = await self.client.post("/api/invoke/local_poster_candidates", json={"path": str(self.video)})
        self.assertEqual(response.status_code, 200, response.text)
        candidates = response.json()
        self.assertEqual([item["seconds"] for item in candidates], [0, 2, 5, 10, 30, 60], "one candidate per offered frame time")
        # The fixture is 8 seconds long, so frames past its end have no image instead of failing the list.
        rendered = {item["seconds"] for item in candidates if item["image"]}
        self.assertEqual(rendered, {0, 2, 5}, "frames inside the clip are rendered")
        self.assertTrue(all(item["image"] is None for item in candidates if item["seconds"] >= 10))
        import base64
        for item in candidates:
            if item["image"]:
                self.assertTrue(base64.b64decode(item["image"]).startswith(b"\xff\xd8"), f"{item['seconds']}s frame is a JPEG")
        self.assertEqual(len(list((self.data / "thumbnails").glob("*.jpg"))), 3, "candidates share the poster cache")
        self.assertEqual(list((self.data / "thumbnails").glob(".*.jpg")), [], "no temporary frames are left behind")

        outside = Path(tempfile.mkdtemp(prefix="lumen-outside-"))
        self.addCleanup(shutil.rmtree, outside, True)
        blocked = await self.client.post("/api/invoke/local_poster_candidates", json={"path": str(outside / "x.mp4")})
        self.assertEqual(blocked.status_code, 404, "candidates honour the local media roots")

    async def test_poster_and_filmstrip_refuse_videos_outside_allowed_roots(self):
        outside_dir = Path(tempfile.mkdtemp(prefix="lumen-outside-"))
        self.addCleanup(shutil.rmtree, outside_dir, True)
        outside = outside_dir / "outside.mp4"
        outside.write_bytes(self.video.read_bytes())
        for endpoint in ("local-thumbnail", "local-filmstrip"):
            response = await self.client.get(f"/api/{endpoint}?path={outside}&client=local-preview")
            self.assertEqual(response.status_code, 404, endpoint)
        self.assertFalse((self.data / "thumbnails").exists(), "rejected paths never reach ffmpeg or the cache")

    async def test_thumbnail_route_keeps_browser_bridge_protection(self):
        for endpoint in ("local-thumbnail", "local-filmstrip"):
            path = f"/api/{endpoint}?path={self.video}&client=local-preview"
            bad_origin = await self.client.get(path, headers={"Origin": "https://attacker.invalid"})
            self.assertEqual(bad_origin.status_code, 403)


if __name__ == "__main__":
    unittest.main()

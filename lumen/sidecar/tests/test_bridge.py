import importlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import httpx


class BridgeTests(unittest.IsolatedAsyncioTestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="lumen-tests-")
        os.environ["LUMEN_DATA_DIR"] = cls.temp.name
        os.environ["LUMEN_STANDALONE"] = "1"
        os.environ["LUMEN_SEED_CATALOG"] = "0"
        from app import main, bridge
        cls.main = main
        cls.bridge = bridge

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    async def request(self, path, body=None, headers=None):
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=self.main.app), base_url="http://127.0.0.1:43127") as client:
            return await client.post(path, json=body or {}, headers=headers or {})

    async def test_bridge_requires_header_and_rejects_foreign_origin(self):
        path = "/api/invoke/list_playlists_cmd"
        self.assertEqual((await self.request(path)).status_code, 403)
        good = {"X-Lumen-Client": "local-preview", "Origin": "http://127.0.0.1:1420"}
        self.assertEqual((await self.request(path, headers=good)).status_code, 200)
        bad = {**good, "Origin": "https://attacker.example"}
        self.assertEqual((await self.request(path, headers=bad)).status_code, 403)
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=self.main.app), base_url="http://attacker.example:43127") as client:
            self.assertEqual((await client.post(path, json={}, headers=good)).status_code, 403)

    async def test_settings_import_favorites_refresh_and_delete(self):
        headers = {"X-Lumen-Client": "local-preview"}
        r = await self.request("/api/invoke/set_setting_cmd", {"key": "cache", "value": "no"}, headers)
        self.assertEqual(r.status_code, 200)
        r = await self.request("/api/invoke/get_settings", headers=headers)
        self.assertEqual(r.json()["cache"], "no")
        playlist = Path(self.temp.name) / "test.m3u"
        playlist.write_text('#EXTM3U\n#EXTINF:-1 group-title="News, Local",World News, HD\nhttps://example.org/news.m3u8\n')
        r = await self.request("/api/invoke/import_playlist_file", {"path": str(playlist)}, headers)
        self.assertEqual(r.status_code, 200)
        pid = r.json()
        rows = (await self.request("/api/invoke/list_channels_cmd", {"playlistId": pid}, headers)).json()
        self.assertEqual(rows[0]["name"], "World News, HD")
        self.assertEqual(rows[0]["group_title"], "News, Local")
        cid = rows[0]["id"]
        self.assertTrue((await self.request("/api/invoke/toggle_favorite_cmd", {"channelId": cid}, headers)).json())
        self.assertIn(cid, (await self.request("/api/invoke/list_favorites_cmd", headers=headers)).json())
        self.assertEqual((await self.request("/api/invoke/delete_playlist_cmd", {"playlistId": pid}, headers)).status_code, 200)
        self.assertNotIn(cid, (await self.request("/api/invoke/list_favorites_cmd", headers=headers)).json())
        self.assertTrue(playlist.exists(), "Deleting a source must not delete its original file")

    async def test_local_thumbnail_endpoint_returns_valid_jpeg(self):
        ffmpeg = shutil.which("ffmpeg")
        if ffmpeg is None:
            self.skipTest("ffmpeg is required to generate a local thumbnail fixture")

        video = Path(self.temp.name) / "thumbnail-fixture.mp4"
        generated = subprocess.run(
            [
                ffmpeg, "-nostdin", "-hide_banner", "-loglevel", "error",
                "-f", "lavfi", "-i", "color=c=blue:s=64x64:d=1",
                "-frames:v", "1", "-y", str(video),
            ],
            check=False,
            capture_output=True,
            timeout=15,
        )
        self.assertEqual(generated.returncode, 0, generated.stderr.decode("utf-8", "replace"))

        async with httpx.AsyncClient(
            transport=httpx.ASGITransport(app=self.main.app),
            base_url="http://127.0.0.1:43127",
        ) as client:
            response = await client.get(
                "/api/local-thumbnail",
                params={"path": str(video), "client": "local-preview"},
            )

        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual(response.headers.get("content-type"), "image/jpeg")
        self.assertTrue(response.content.startswith(bytes([0xFF, 0xD8])), "the endpoint must return JPEG bytes")
        self.assertLessEqual(len(response.content), 2_000_000)

    async def test_progress_for_deleted_channel_does_not_fail_foreign_keys(self):
        """Stale channel ids must degrade gracefully, never raise sqlite3.IntegrityError."""
        headers = {"X-Lumen-Client": "local-preview"}
        playlist = Path(self.temp.name) / "doomed.m3u"
        playlist.write_text('#EXTM3U\n#EXTINF:-1,Doomed Channel\nhttps://example.org/doomed.m3u8\n')
        pid = (await self.request("/api/invoke/import_playlist_file", {"path": str(playlist)}, headers)).json()
        cid = (await self.request("/api/invoke/list_channels_cmd", {"playlistId": pid}, headers)).json()[0]["id"]

        self.assertEqual((await self.request("/api/invoke/play_url", {"url": "https://example.org/doomed.m3u8", "channelId": cid}, headers)).status_code, 200)
        self.assertEqual((await self.request("/api/invoke/save_progress_cmd", {"channelId": cid, "path": "https://example.org/doomed.m3u8", "position": 3.5, "duration": 60.0}, headers)).status_code, 200)

        # Removing the source deletes the channel those history rows point at.
        self.assertEqual((await self.request("/api/invoke/delete_playlist_cmd", {"playlistId": pid}, headers)).status_code, 200)

        self.assertEqual((await self.request("/api/invoke/play_url", {"url": "https://example.org/doomed.m3u8", "channelId": cid}, headers)).status_code, 200)
        self.assertEqual((await self.request("/api/invoke/save_progress_cmd", {"channelId": cid, "path": "https://example.org/doomed.m3u8", "position": 9.0, "duration": 60.0}, headers)).status_code, 200)
        self.assertEqual((await self.request("/api/invoke/toggle_favorite_cmd", {"channelId": cid}, headers)).status_code, 404)

    async def test_zero_volume_is_not_replaced_by_default(self):
        with patch.object(self.bridge.mpv_controller, "set_volume") as setter:
            response = await self.request("/api/invoke/set_volume", {"vol": 0}, {"X-Lumen-Client": "local-preview"})
            self.assertEqual(response.status_code, 200)
            setter.assert_called_once_with(0)

    async def test_native_sidecar_has_no_browser_bridge_and_requires_token(self):
        with patch.dict(os.environ, {"LUMEN_STANDALONE": "0"}):
            native = importlib.reload(self.main)
            async with httpx.AsyncClient(transport=httpx.ASGITransport(app=native.app), base_url="http://127.0.0.1:43127") as client:
                response = await client.post("/api/invoke/list_playlists_cmd", json={}, headers={"X-Lumen-Client": "local-preview"})
                self.assertEqual(response.status_code, 404)
                self.assertEqual((await client.get("/health")).status_code, 401)
                self.assertEqual((await client.get("/health", headers={"Authorization": f"Bearer {native.TOKEN}"})).status_code, 200)
        importlib.reload(self.main)


if __name__ == "__main__":
    unittest.main()

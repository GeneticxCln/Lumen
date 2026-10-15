import importlib
import os
from pathlib import Path
import shutil
import sqlite3
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
        os.environ["LUMEN_LOCAL_MEDIA_ROOTS"] = cls.temp.name
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

    async def test_search_preferred_language_setting_round_trips(self):
        headers = {"X-Lumen-Client": "local-preview"}
        r = await self.request("/api/invoke/set_setting_cmd", {"key": "search_preferred_language", "value": "fr"}, headers)
        self.assertEqual(r.status_code, 200)
        after = (await self.request("/api/invoke/get_settings", headers=headers)).json()
        self.assertEqual(after["search_preferred_language"], "fr")
        r = await self.request("/api/invoke/set_setting_cmd", {"key": "search_hide_other_languages", "value": "true"}, headers)
        self.assertEqual(r.status_code, 200)
        after = (await self.request("/api/invoke/get_settings", headers=headers)).json()
        self.assertEqual(after["search_hide_other_languages"], "true")

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

    async def test_local_media_route_only_serves_media_inside_allowed_roots(self):
        headers = {"X-Lumen-Client": "local-preview"}
        inside = Path(self.temp.name) / "clip.mp3"
        inside.write_bytes(b"ID3 test")
        notes = Path(self.temp.name) / "notes.txt"
        notes.write_text("private")
        outside_dir = tempfile.TemporaryDirectory(prefix="lumen-outside-")
        self.addCleanup(outside_dir.cleanup)
        outside = Path(outside_dir.name) / "secret.mp3"
        outside.write_bytes(b"ID3 outside")

        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=self.main.app), base_url="http://127.0.0.1:43127") as client:
            ok = await client.get("/api/local-media", params={"path": str(inside), "client": "local-preview"})
            self.assertEqual(ok.status_code, 200)
            self.assertEqual(ok.content, b"ID3 test")
            for blocked in (outside, notes, Path(self.temp.name) / "missing.mp3"):
                response = await client.get("/api/local-media", params={"path": str(blocked), "client": "local-preview"})
                self.assertEqual(response.status_code, 404, str(blocked))

    def test_library_search_splits_multi_word_queries(self):
        conn = sqlite3.connect(":memory:")
        conn.row_factory = sqlite3.Row
        conn.execute(
            "CREATE TABLE channels (id INTEGER PRIMARY KEY, playlist_id INTEGER, name TEXT, url TEXT, "
            "group_title TEXT, tvg_id TEXT, language TEXT, extra_headers TEXT, tmdb_type TEXT, release_year INTEGER)"
        )
        conn.executemany(
            "INSERT INTO channels (playlist_id, name, url) VALUES (1, ?, 'https://example.org/x')",
            [("Breaking Bad",), ("Bad Breaking Fan",), ("Unrelated",)],
        )
        names = sorted(row["name"] for row in self.bridge._search_library(conn, "breaking   bad"))
        self.assertEqual(names, ["Bad Breaking Fan", "Breaking Bad"])
        conn.close()

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

    async def test_profiles_scope_favorites_and_history(self):
        headers = {"X-Lumen-Client": "local-preview"}
        profiles = (await self.request("/api/invoke/list_profiles_cmd", headers=headers)).json()
        self.assertEqual(profiles[0]["name"], "Lumen")
        home_id = profiles[0]["id"]

        created = await self.request("/api/invoke/create_profile_cmd", {"name": "  Kids  "}, headers)
        self.assertEqual(created.status_code, 200)
        kids = created.json()
        self.assertEqual(kids["name"], "Kids")
        self.assertFalse(kids["has_pin"])

        playlist = Path(self.temp.name) / "profiles.m3u"
        playlist.write_text("#EXTM3U\n#EXTINF:-1,Profile Channel\nhttps://example.org/profile.m3u8\n")
        pid = (await self.request("/api/invoke/import_playlist_file", {"path": str(playlist)}, headers)).json()
        cid = (await self.request("/api/invoke/list_channels_cmd", {"playlistId": pid}, headers)).json()[0]["id"]
        try:
            self.assertEqual((await self.request("/api/invoke/activate_profile_cmd", {"profileId": kids["id"]}, headers)).status_code, 200)
            self.assertTrue((await self.request("/api/invoke/toggle_favorite_cmd", {"channelId": cid}, headers)).json())
            self.assertEqual((await self.request("/api/invoke/save_progress_cmd", {"channelId": cid, "path": "https://example.org/profile.m3u8", "position": 4.0, "duration": 60.0}, headers)).status_code, 200)
            self.assertEqual((await self.request("/api/invoke/list_favorites_cmd", headers=headers)).json(), [cid])
            kids_history = (await self.request("/api/invoke/get_history_cmd", headers=headers)).json()
            self.assertIn("https://example.org/profile.m3u8", [row["path"] for row in kids_history])

            active = (await self.request("/api/invoke/activate_profile_cmd", {"profileId": home_id}, headers)).json()
            self.assertEqual(active["id"], home_id)
            self.assertEqual((await self.request("/api/invoke/active_profile_cmd", headers=headers)).json()["id"], home_id)
            self.assertNotIn(cid, (await self.request("/api/invoke/list_favorites_cmd", headers=headers)).json())
            home_history = (await self.request("/api/invoke/get_history_cmd", headers=headers)).json()
            self.assertNotIn("https://example.org/profile.m3u8", [row["path"] for row in home_history])
        finally:
            await self.request("/api/invoke/activate_profile_cmd", {"profileId": home_id}, headers)
            await self.request("/api/invoke/delete_profile_cmd", {"profileId": kids["id"]}, headers)
            await self.request("/api/invoke/delete_playlist_cmd", {"playlistId": pid}, headers)

    async def test_profile_pin_is_enforced_here_and_cannot_be_set_in_browser(self):
        headers = {"X-Lumen-Client": "local-preview"}
        home_id = (await self.request("/api/invoke/list_profiles_cmd", headers=headers)).json()[0]["id"]
        locked = (await self.request("/api/invoke/create_profile_cmd", {"name": "Locked"}, headers)).json()
        pid = locked["id"]
        try:
            # Argon2 hashes are made by the desktop app, so the browser bridge must refuse to set a PIN.
            refused = await self.request("/api/invoke/set_profile_pin_cmd", {"profileId": pid, "pin": "1234"}, headers)
            self.assertEqual(refused.status_code, 400)

            # A PIN set on desktop keeps the profile locked here.
            with self.bridge.get_conn() as conn:
                conn.execute("UPDATE profiles SET pin_hash = 'argon2-hash-from-desktop' WHERE id = ?", (pid,))
            listed = [p for p in (await self.request("/api/invoke/list_profiles_cmd", headers=headers)).json() if p["id"] == pid]
            self.assertTrue(listed[0]["has_pin"])
            self.assertEqual((await self.request("/api/invoke/activate_profile_cmd", {"profileId": pid}, headers)).status_code, 403)
            self.assertEqual((await self.request("/api/invoke/active_profile_cmd", headers=headers)).json()["id"], home_id)

            # Clearing the PIN is allowed, as on desktop.
            cleared = await self.request("/api/invoke/set_profile_pin_cmd", {"profileId": pid}, headers)
            self.assertEqual(cleared.status_code, 200)
            self.assertFalse(cleared.json()["has_pin"])
            self.assertEqual((await self.request("/api/invoke/activate_profile_cmd", {"profileId": pid}, headers)).status_code, 200)
        finally:
            await self.request("/api/invoke/activate_profile_cmd", {"profileId": home_id}, headers)
            await self.request("/api/invoke/delete_profile_cmd", {"profileId": pid}, headers)

    def test_legacy_single_profile_database_is_adopted_by_first_profile(self):
        conn = sqlite3.connect(":memory:")
        conn.row_factory = sqlite3.Row
        conn.executescript(
            """
            CREATE TABLE channels (id INTEGER PRIMARY KEY);
            CREATE TABLE profiles (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, color TEXT, pin_hash TEXT, created_at INTEGER NOT NULL, last_used_at INTEGER NOT NULL);
            CREATE TABLE history (id INTEGER PRIMARY KEY AUTOINCREMENT, channel_id INTEGER, path TEXT NOT NULL, position REAL NOT NULL, duration REAL NOT NULL, played_at INTEGER NOT NULL);
            CREATE TABLE favorites (channel_id INTEGER PRIMARY KEY);
            INSERT INTO channels (id) VALUES (7);
            INSERT INTO history (channel_id, path, position, duration, played_at) VALUES (7, 'https://example.org/a.m3u8', 1.0, 60.0, 100);
            INSERT INTO favorites (channel_id) VALUES (7);
            """
        )
        self.bridge.migrate_profiles(conn)
        self.bridge.migrate_profiles(conn)  # a second run must change nothing
        self.assertEqual([(r["id"], r["name"]) for r in conn.execute("SELECT id, name FROM profiles")], [(1, "Lumen")])
        self.assertEqual([r["profile_id"] for r in conn.execute("SELECT profile_id FROM history")], [1])
        self.assertEqual([(r["profile_id"], r["channel_id"]) for r in conn.execute("SELECT profile_id, channel_id FROM favorites")], [(1, 7)])
        conn.close()


if __name__ == "__main__":
    unittest.main()

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path

# The bridge creates its database at import time, so point it at a scratch directory first.
# Its routes are only mounted when LUMEN_STANDALONE=1 is set before app.main is imported.
os.environ["LUMEN_DATA_DIR"] = tempfile.mkdtemp(prefix="lumen-tmdb-test-")
os.environ["LUMEN_STANDALONE"] = "1"
os.environ["LUMEN_SEED_CATALOG"] = "0"
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import httpx  # noqa: E402

from app import main  # noqa: E402
from app import bridge  # noqa: E402

HEADERS = {"X-Lumen-Client": "local-preview"}
ENV_NAME = "LUMEN_TMDB_API_KEY"


async def invoke(cmd: str, body: dict | None = None) -> httpx.Response:
    transport = httpx.ASGITransport(app=main.app)
    async with httpx.AsyncClient(transport=transport, base_url="http://127.0.0.1:43127") as client:
        return await client.post(f"/api/invoke/{cmd}", json=body or {}, headers=HEADERS)


def _settings_row() -> str | None:
    with bridge.get_conn() as conn:
        row = conn.execute("SELECT value FROM settings WHERE key = 'tmdb_api_key'").fetchone()
    return row["value"] if row else None


class TmdbKeyTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self) -> None:
        self._saved_env = os.environ.pop(ENV_NAME, None)
        bridge._TMDB_KEY = ""
        with bridge.get_conn() as conn:
            conn.execute("DELETE FROM settings WHERE key = 'tmdb_api_key'")
            conn.commit()

    def tearDown(self) -> None:
        os.environ.pop(ENV_NAME, None)
        if self._saved_env is not None:
            os.environ[ENV_NAME] = self._saved_env

    async def test_saved_key_stays_in_memory_and_is_never_returned(self) -> None:
        r = await invoke("set_tmdb_api_key", {"key": "  tmdb-secret  "})
        self.assertEqual(r.status_code, 200)
        self.assertEqual(bridge._TMDB_KEY, "tmdb-secret")
        self.assertIsNone(_settings_row())

        status = (await invoke("tmdb_status")).json()
        self.assertTrue(status["configured"])
        self.assertEqual(status["key_source"], "memory")
        self.assertNotIn("tmdb-secret", str(status))

    async def test_blank_key_clears_the_saved_key(self) -> None:
        await invoke("set_tmdb_api_key", {"key": "tmdb-secret"})
        await invoke("set_tmdb_api_key", {"key": "   "})

        status = (await invoke("tmdb_status")).json()
        self.assertFalse(status["configured"])
        self.assertEqual(status["key_source"], "none")

    async def test_non_string_key_is_rejected(self) -> None:
        r = await invoke("set_tmdb_api_key", {"key": 42})
        self.assertEqual(r.status_code, 400)
        self.assertEqual(bridge._TMDB_KEY, "")

    async def test_generic_setter_refuses_the_tmdb_key(self) -> None:
        r = await invoke("set_setting_cmd", {"key": "tmdb_api_key", "value": "tmdb-secret"})
        self.assertEqual(r.status_code, 400)
        self.assertIsNone(_settings_row())
        self.assertEqual(bridge._TMDB_KEY, "")

    async def test_legacy_plaintext_row_is_moved_to_memory_and_deleted(self) -> None:
        with bridge.get_conn() as conn:
            conn.execute("INSERT INTO settings (key, value) VALUES ('tmdb_api_key', 'legacy-key')")
            conn.commit()

        status = (await invoke("tmdb_status")).json()

        self.assertIsNone(_settings_row(), "legacy plaintext row must be removed")
        self.assertEqual(bridge._TMDB_KEY, "legacy-key")
        self.assertTrue(status["configured"])
        self.assertEqual(status["key_source"], "memory")

    async def test_environment_key_takes_precedence(self) -> None:
        await invoke("set_tmdb_api_key", {"key": "tmdb-secret"})
        os.environ[ENV_NAME] = "env-key"

        status = (await invoke("tmdb_status")).json()
        self.assertEqual(status["key_source"], "env")


if __name__ == "__main__":
    unittest.main()

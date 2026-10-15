from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

# The bridge creates its database at import time, so point it at a scratch directory first.
os.environ["LUMEN_DATA_DIR"] = tempfile.mkdtemp(prefix="lumen-test-")
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from fastapi import HTTPException  # noqa: E402

from app import bridge  # noqa: E402


def _provider(provider_id: str, **extra):
    return {"id": provider_id, "name": provider_id.title(), "url": "https://example.com/api", **extra}


class SearchProviderTokenTests(unittest.TestCase):
    def setUp(self) -> None:
        bridge._PROVIDER_TOKENS.clear()
        with bridge.get_conn() as conn:
            conn.execute("DELETE FROM settings WHERE key = 'search_providers'")
            conn.commit()

    def _stored_row(self) -> str:
        with bridge.get_conn() as conn:
            row = conn.execute("SELECT value FROM settings WHERE key = 'search_providers'").fetchone()
        return row["value"] if row else ""

    def test_token_is_kept_in_memory_and_not_persisted(self) -> None:
        returned = bridge._save_search_providers([_provider("catalog", token="s3cret")])

        self.assertNotIn("token", returned[0])
        self.assertNotIn("s3cret", self._stored_row())
        self.assertNotIn("token", json.loads(self._stored_row())[0])
        self.assertEqual(bridge._PROVIDER_TOKENS, {"catalog": "s3cret"})

    def test_search_path_merges_memory_token_back_in(self) -> None:
        bridge._save_search_providers([_provider("catalog", token="s3cret")])
        with bridge.get_conn() as conn:
            merged = bridge._with_memory_tokens(bridge._stored_providers(conn))

        self.assertEqual(merged[0]["token"], "s3cret")

    def test_missing_token_keeps_the_saved_one(self) -> None:
        bridge._save_search_providers([_provider("catalog", token="s3cret")])
        bridge._save_search_providers([_provider("catalog", url="https://example.com/v2")])

        self.assertEqual(bridge._PROVIDER_TOKENS.get("catalog"), "s3cret")

    def test_empty_token_clears_and_removed_provider_drops_its_token(self) -> None:
        bridge._save_search_providers([_provider("catalog", token="s3cret"), _provider("other", token="t2")])
        bridge._save_search_providers([_provider("catalog", token="")])
        self.assertNotIn("catalog", bridge._PROVIDER_TOKENS)
        self.assertNotIn("other", bridge._PROVIDER_TOKENS)

    def test_rejects_providers_without_an_id_or_with_a_non_string_token(self) -> None:
        with self.assertRaises(HTTPException):
            bridge._save_search_providers([{"name": "no id"}])
        with self.assertRaises(HTTPException):
            bridge._save_search_providers([_provider("catalog", token=42)])


if __name__ == "__main__":
    unittest.main()

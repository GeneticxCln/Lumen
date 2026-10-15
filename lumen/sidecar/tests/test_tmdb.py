"""Tests for TMDB enrichment.

Nothing here touches the network, the database, or a real API key: the pure
helpers are tested directly and the HTTP orchestration runs against an
``httpx.MockTransport``.
"""

import unittest

import httpx

from app import tmdb


class SearchStub:
    """Routes mock-transport requests to canned TMDB payloads and records them."""

    def __init__(self, movie_hits=None, tv_hits=None, details=None, status=200):
        self.movie_hits = movie_hits or []
        self.tv_hits = tv_hits or []
        self.details = details or {}
        self.status = status
        self.calls = []

    def __call__(self, request: httpx.Request) -> httpx.Response:
        self.calls.append(str(request.url))
        if self.status != 200:
            return httpx.Response(self.status, json={"status_message": "rejected"})
        path = request.url.path
        if path.endswith("/search/movie"):
            return httpx.Response(200, json={"results": self.movie_hits})
        if path.endswith("/search/tv"):
            return httpx.Response(200, json={"results": self.tv_hits})
        if "/movie/" in path or "/tv/" in path:
            return httpx.Response(200, json=self.details)
        return httpx.Response(404, json={})


def client_for(stub: SearchStub) -> httpx.AsyncClient:
    return httpx.AsyncClient(transport=httpx.MockTransport(stub))


class TitleGuessingTests(unittest.TestCase):
    def test_clean_title_strips_brackets_quality_markers_and_trailing_junk(self):
        cases = {
            "BFM Business (1080p)": "BFM Business",
            "Big Buck Bunny (Open Movie HLS)": "Big Buck Bunny",
            "Tears of Steel (Blender Open Movie)": "Tears of Steel",
            "Online TV Nusantara (720p) [Not 24/7]": "Online TV Nusantara",
            "Red Bull TV HD": "Red Bull TV",
            "NHK World-Japan HD (720p)": "NHK World-Japan",
            "Some Channel 4K -": "Some Channel",
        }
        for raw, expected in cases.items():
            self.assertEqual(tmdb.clean_title(raw), expected, raw)

    def test_clean_title_handles_empty_and_bracket_only_names(self):
        self.assertEqual(tmdb.clean_title(None), "")
        self.assertEqual(tmdb.clean_title(""), "")
        self.assertEqual(tmdb.clean_title("(1080p)"), "")

    def test_quality_markers_are_removed_as_whole_words_only(self):
        # "HD" inside a larger word must survive; a standalone marker must not.
        self.assertEqual(tmdb.clean_title("HDNet Movies"), "HDNet Movies")
        self.assertEqual(tmdb.clean_title("Movies HD"), "Movies")


class NormalizeAndMatchTests(unittest.TestCase):
    def test_normalize_drops_punctuation_and_leading_article(self):
        self.assertEqual(tmdb.normalize_title("The  Big Buck Bunny!"), "big buck bunny")
        self.assertEqual(tmdb.normalize_title("NHK World-Japan"), "nhk world japan")
        self.assertEqual(tmdb.normalize_title(None), "")

    def test_match_accepts_exact_titles(self):
        self.assertTrue(tmdb.titles_match("Big Buck Bunny", "big  buck bunny!!"))
        self.assertTrue(tmdb.titles_match("The Matrix", "Matrix"))

    def test_match_accepts_containment_for_a_long_enough_query(self):
        self.assertTrue(tmdb.titles_match("Tears of Steel", "Tears of Steel (Blender)"))

    def test_match_rejects_junk_and_short_queries(self):
        self.assertFalse(tmdb.titles_match("BFM Business", "Completely Different Film"))
        self.assertFalse(tmdb.titles_match("De", "Dexter"))
        self.assertFalse(tmdb.titles_match("", "Anything"))
        self.assertFalse(tmdb.titles_match("Anything", None))


class AuthChoiceTests(unittest.TestCase):
    def test_jwt_uses_bearer_header_and_v3_key_uses_query_param(self):
        self.assertTrue(tmdb.uses_bearer_token("eyJhbGciOiJIUzI1NiJ9.abc"))
        self.assertFalse(tmdb.uses_bearer_token("d2d0a1f9c4b74e1e8a3f5c6d7e8f9a0b"))

        jwt_kwargs = tmdb.auth_request_kwargs("eyJhbGciOiJIUzI1NiJ9.abc")
        self.assertEqual(jwt_kwargs["headers"]["Authorization"], "Bearer eyJhbGciOiJIUzI1NiJ9.abc")
        self.assertEqual(jwt_kwargs["params"], {})

        key_kwargs = tmdb.auth_request_kwargs("abc123")
        self.assertEqual(key_kwargs["headers"], {})
        self.assertEqual(key_kwargs["params"], {"api_key": "abc123"})

    def test_empty_key_uses_neither_scheme(self):
        self.assertFalse(tmdb.uses_bearer_token(""))
        self.assertEqual(tmdb.auth_request_kwargs("")["params"], {"api_key": ""})


class RowMappingTests(unittest.TestCase):
    def test_movie_payload_maps_to_channel_columns(self):
        updates = tmdb.build_row_updates("movie", {
            "id": 42,
            "poster_path": "/bbb.jpg",
            "backdrop_path": "/bbb-bd.jpg",
            "overview": "A big rabbit.",
            "release_date": "2008-05-20",
            "vote_average": 7.44,
            "genres": [{"id": 16, "name": "Animation"}, {"id": 35, "name": "Comedy"}],
        })
        self.assertEqual(updates["tmdb_id"], 42)
        self.assertEqual(updates["tmdb_type"], "movie")
        self.assertEqual(updates["release_year"], 2008)
        self.assertEqual(updates["rating"], 7.4)
        self.assertEqual(updates["genre_names"], "Animation;Comedy")

    def test_tv_payload_uses_first_air_date_and_tolerates_missing_fields(self):
        updates = tmdb.build_row_updates("tv", {"id": 7, "first_air_date": "2019-03-01"})
        self.assertEqual(updates["release_year"], 2019)
        self.assertIsNone(updates["poster_path"])
        self.assertIsNone(updates["genre_names"])
        self.assertIsNone(updates["rating"])

    def test_unparseable_dates_and_genreless_payloads_are_null(self):
        updates = tmdb.build_row_updates("movie", {"id": 1, "release_date": "", "genres": []})
        self.assertIsNone(updates["release_year"])
        self.assertIsNone(updates["genre_names"])


class EnrichTests(unittest.IsolatedAsyncioTestCase):
    async def test_unmatched_candidates_are_still_stamped_and_counts_add_up(self):
        """A row that finds no match must be marked attempted, never re-queried."""
        stub = SearchStub(
            movie_hits=[{"id": 9, "title": "Something Else Entirely", "name": "Something Else Entirely"}],
            tv_hits=[{"id": 9, "title": "Something Else Entirely", "name": "Something Else Entirely"}],
        )
        client = client_for(stub)
        stored = {}
        candidates = [(1, "BFM Business (1080p)"), (2, "ZillarBarta News")]

        stats = await tmdb.enrich_channels(
            "key123", candidates, lambda cid, upd: stored.setdefault(cid, upd),
            client=client, delay=0, now=1000,
        )
        await client.aclose()

        self.assertEqual(stats["enriched"], 0)
        self.assertEqual(stats["skipped"], 2)
        self.assertEqual(stats["enriched"] + stats["skipped"], 2)
        for cid in (1, 2):
            self.assertEqual(stored[cid]["enriched_at"], 1000)
            self.assertNotIn("tmdb_id", stored[cid])
        # Two searches (movie then tv) per candidate.
        self.assertEqual(stats["requests"], 4)

    async def test_matching_title_stores_full_metadata(self):
        stub = SearchStub(
            movie_hits=[{"id": 42, "title": "Big Buck Bunny", "name": "Big Buck Bunny"}],
            details={
                "id": 42,
                "title": "Big Buck Bunny",
                "poster_path": "/bbb.jpg",
                "backdrop_path": "/bbb-bd.jpg",
                "overview": "A big rabbit.",
                "release_date": "2008-05-20",
                "vote_average": 7.44,
                "genres": [{"id": 16, "name": "Animation"}, {"id": 35, "name": "Comedy"}],
            },
        )
        client = client_for(stub)
        stored = {}

        stats = await tmdb.enrich_channels(
            "key123", [(3, "Big Buck Bunny (Open Movie HLS)")],
            lambda cid, upd: stored.setdefault(cid, upd), client=client, delay=0, now=2000,
        )
        await client.aclose()

        self.assertEqual(stats["enriched"], 1)
        self.assertEqual(stats["skipped"], 0)
        row = stored[3]
        self.assertEqual(row["tmdb_id"], 42)
        self.assertEqual(row["tmdb_type"], "movie")
        self.assertEqual(row["poster_path"], "/bbb.jpg")
        self.assertEqual(row["release_year"], 2008)
        self.assertEqual(row["rating"], 7.4)
        self.assertEqual(row["genre_names"], "Animation;Comedy")
        self.assertEqual(row["enriched_at"], 2000)
        # A v3 key travels as a query param, not a bearer header.
        self.assertIn("api_key=key123", stub.calls[0])

    async def test_tv_is_searched_when_movies_do_not_match(self):
        stub = SearchStub(
            movie_hits=[{"id": 1, "title": "Unrelated Film", "name": "Unrelated Film"}],
            tv_hits=[{"id": 55, "title": "Some Show", "name": "Some Show"}],
            details={"id": 55, "name": "Some Show", "first_air_date": "2015-01-01",
                     "genres": [{"id": 10763, "name": "News"}]},
        )
        client = client_for(stub)
        stored = {}

        stats = await tmdb.enrich_channels(
            "key123", [(4, "Some Show")],
            lambda cid, upd: stored.setdefault(cid, upd), client=client, delay=0, now=3000,
        )
        await client.aclose()

        self.assertEqual(stats["enriched"], 1)
        self.assertEqual(stored[4]["tmdb_type"], "tv")
        self.assertEqual(stored[4]["tmdb_id"], 55)
        self.assertEqual(stored[4]["genre_names"], "News")

    async def test_rejected_key_aborts_the_run(self):
        stub = SearchStub(status=401)
        client = client_for(stub)
        stored = {}

        with self.assertRaises(tmdb.TmdbAuthError):
            await tmdb.enrich_channels(
                "bad-key", [(5, "Big Buck Bunny")],
                lambda cid, upd: stored.setdefault(cid, upd), client=client, delay=0,
            )
        await client.aclose()

        self.assertEqual(stored, {}, "Nothing should be persisted after a rejected key")

    async def test_a_single_failure_does_not_abort_the_batch(self):
        """One broken response costs one candidate, not the whole run."""
        state = {"n": 0}

        def handler(request: httpx.Request) -> httpx.Response:
            state["n"] += 1
            if state["n"] == 1:
                return httpx.Response(500, json={})
            return httpx.Response(200, json={"results": []})

        client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
        stored = {}

        stats = await tmdb.enrich_channels(
            "key123", [(6, "First Channel"), (7, "Second Channel")],
            lambda cid, upd: stored.setdefault(cid, upd), client=client, delay=0, now=4000,
        )
        await client.aclose()

        self.assertEqual(stats["skipped"], 2)
        self.assertEqual(set(stored), {6, 7})
        self.assertEqual(stored[6]["enriched_at"], 4000)

    async def test_blank_names_skip_the_api_but_still_get_stamped(self):
        stub = SearchStub()
        client = client_for(stub)
        stored = {}

        stats = await tmdb.enrich_channels(
            "key123", [(8, "(1080p)")],
            lambda cid, upd: stored.setdefault(cid, upd), client=client, delay=0, now=5000,
        )
        await client.aclose()

        self.assertEqual(stats["requests"], 0, "No API call for a title that cannot exist")
        self.assertEqual(stats["skipped"], 1)
        self.assertEqual(stored[8]["enriched_at"], 5000)


if __name__ == "__main__":
    unittest.main()

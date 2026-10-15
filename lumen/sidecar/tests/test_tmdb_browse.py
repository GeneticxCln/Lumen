import unittest

import httpx

from app import tmdb


GENRES_MOVIE = {"genres": [{"id": 28, "name": "Action"}, {"id": 18, "name": "Drama"}]}
GENRES_TV = {"genres": [{"id": 10759, "name": "Action & Adventure"}]}


class BrowseEntriesTests(unittest.TestCase):
    def test_normalizes_and_dedupes_in_priority_order(self):
        trending = {
            "results": [
                {"id": 1, "media_type": "movie", "title": "Alpha", "release_date": "2024-03-01",
                 "poster_path": "/a.jpg", "backdrop_path": "/ab.jpg", "overview": "Trending film",
                 "vote_average": 7.456, "genre_ids": [28, 999]},
                {"id": 50, "media_type": "person", "name": "Not a title"},
                {"id": 2, "media_type": "tv", "name": "Beta", "first_air_date": "2021-09-10",
                 "vote_average": 8.0, "genre_ids": [10759]},
            ]
        }
        movies = {"results": [
            {"id": 1, "title": "Alpha", "release_date": "2024-03-01"},
            {"id": 3, "title": "Gamma", "release_date": "", "vote_average": None, "genre_ids": []},
        ]}
        shows = {"results": [{"id": 2, "name": "Beta duplicate"}, {"id": 4, "name": "Delta", "first_air_date": "1999-01-01"}]}

        rows = tmdb.browse_entries(trending, movies, shows, GENRES_MOVIE, GENRES_TV)

        self.assertEqual([(r["tmdb_type"], r["tmdb_id"]) for r in rows], [
            ("movie", 1), ("tv", 2), ("movie", 3), ("tv", 4),
        ])
        alpha = rows[0]
        self.assertEqual(alpha["name"], "Alpha")
        self.assertEqual(alpha["release_year"], 2024)
        self.assertEqual(alpha["rating"], 7.5)
        self.assertEqual(alpha["genre_names"], "Action")
        self.assertEqual(alpha["overview"], "Trending film")
        self.assertEqual(rows[1]["genre_names"], "Action & Adventure")
        self.assertIsNone(rows[2]["release_year"])
        self.assertIsNone(rows[2]["rating"])
        self.assertIsNone(rows[2]["genre_names"])
        self.assertEqual(rows[3]["release_year"], 1999)

    def test_respects_limit(self):
        movies = {"results": [{"id": i, "title": f"Film {i}"} for i in range(10)]}
        rows = tmdb.browse_entries({}, movies, {}, GENRES_MOVIE, GENRES_TV, limit=3)
        self.assertEqual(len(rows), 3)

    def test_skips_rows_without_id_or_name(self):
        movies = {"results": [{"title": "No id"}, {"id": 7}, {"id": "8", "title": "String id"}]}
        self.assertEqual(tmdb.browse_entries({}, movies, {}, GENRES_MOVIE, GENRES_TV), [])


class BrowseClientTests(unittest.IsolatedAsyncioTestCase):
    async def test_browse_requests_five_lists_and_normalizes(self):
        paths = []

        def handler(request: httpx.Request) -> httpx.Response:
            paths.append(request.url.path)
            self.assertEqual(request.headers.get("authorization"), None)
            payloads = {
                "/3/trending/all/week": {"results": [{"id": 9, "media_type": "movie", "title": "Nine"}]},
                "/3/movie/top_rated": {"results": []},
                "/3/tv/top_rated": {"results": []},
                "/3/genre/movie/list": GENRES_MOVIE,
                "/3/genre/tv/list": GENRES_TV,
            }
            return httpx.Response(200, json=payloads[request.url.path])

        transport = httpx.MockTransport(handler)
        async with httpx.AsyncClient(transport=transport) as http:
            client = tmdb.TmdbClient("k" * 32, client=http, delay=0)
            rows = await client.browse()

        self.assertEqual(len(paths), 5)
        self.assertEqual([(r["tmdb_type"], r["tmdb_id"]) for r in rows], [("movie", 9)])


if __name__ == "__main__":
    unittest.main()

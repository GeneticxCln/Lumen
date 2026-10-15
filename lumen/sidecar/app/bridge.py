from __future__ import annotations

import asyncio
import base64
import hashlib
import json
import mimetypes
import os
import re
import shutil
import sqlite3
import subprocess
import time
import uuid
from pathlib import Path
from contextlib import contextmanager
from typing import Any, Dict, List, Optional
from urllib.parse import urlparse

import httpx
from fastapi import APIRouter, HTTPException, Request
from fastapi.responses import FileResponse, Response, StreamingResponse

from . import tmdb
from .extract import extract_stream, read_capped_body
from .harvest import harvest_streams
from .probe import probe_urls
from .search_engine import search_engine

# Browser-preview search tokens and the TMDB key live only in process memory. They are never written to the settings table.
_PROVIDER_TOKENS: Dict[str, str] = {}
_TMDB_KEY: str = ""
_TMDB_KEY_SETTING = "tmdb_api_key"

# Largest playlist accepted from a URL. A bigger response is refused, never buffered.
MAX_PLAYLIST_BYTES = 32 * 1024 * 1024


async def read_capped_text(response: httpx.Response, limit: int = MAX_PLAYLIST_BYTES) -> str:
    """Read a streamed playlist body as text, refusing it once the size passes ``limit``."""
    body = await read_capped_body(response, limit, "Playlist")
    return body.decode(response.encoding or "utf-8", errors="replace")


def _stored_providers(conn: sqlite3.Connection) -> List[Dict[str, Any]]:
    """Provider configuration from settings, with any token field removed."""
    row = conn.execute("SELECT value FROM settings WHERE key = 'search_providers'").fetchone()
    try:
        providers = json.loads(row["value"]) if row else []
    except (TypeError, json.JSONDecodeError):
        return []
    if not isinstance(providers, list):
        return []
    return [{key: value for key, value in provider.items() if key != "token"} for provider in providers if isinstance(provider, dict)]


def _with_memory_tokens(providers: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
    merged = []
    for provider in providers:
        token = _PROVIDER_TOKENS.get(str(provider.get("id", "")))
        merged.append({**provider, "token": token} if token else dict(provider))
    return merged


def _save_search_providers(providers: List[Any]) -> List[Dict[str, Any]]:
    """Keep tokens in memory, persist only the token-free configuration, and return that configuration."""
    stored: List[Dict[str, Any]] = []
    kept_ids = set()
    for provider in providers:
        if not isinstance(provider, dict):
            raise HTTPException(status_code=400, detail="Invalid search providers: each provider must be an object")
        provider_id = str(provider.get("id", "")).strip()
        if not provider_id:
            raise HTTPException(status_code=400, detail="Invalid search providers: each provider needs an id")
        token = provider.get("token")
        if token is not None and not isinstance(token, str):
            raise HTTPException(status_code=400, detail="Invalid search providers: token must be a string")
        # A missing token keeps the saved one. An empty token clears it. A non-empty token replaces it.
        if token == "":
            _PROVIDER_TOKENS.pop(provider_id, None)
        elif token:
            _PROVIDER_TOKENS[provider_id] = token
        kept_ids.add(provider_id)
        stored.append({key: value for key, value in provider.items() if key != "token"})
    for provider_id in [pid for pid in _PROVIDER_TOKENS if pid not in kept_ids]:
        del _PROVIDER_TOKENS[provider_id]
    with get_conn() as conn:
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('search_providers', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            (json.dumps(stored),),
        )
        conn.commit()
    search_engine.clear_cache()
    return stored


router = APIRouter(prefix="/api")

DB_DIR = Path(os.environ.get("LUMEN_DATA_DIR", str(Path.home() / ".local" / "share" / "com.quinton.lumen")))
DB_DIR.mkdir(parents=True, exist_ok=True)
DB_PATH = DB_DIR / "lumen.db"

SCHEMA_SQL = """
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS playlists (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    source TEXT NOT NULL,
    origin TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS channels (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    url TEXT NOT NULL,
    group_title TEXT,
    tvg_id TEXT,
    tvg_logo TEXT,
    language TEXT,
    extra_headers TEXT DEFAULT '{}',
    last_ok INTEGER,
    last_error TEXT,
    tmdb_id INTEGER,
    tmdb_type TEXT,
    poster_path TEXT,
    backdrop_path TEXT,
    overview TEXT,
    release_year INTEGER,
    rating REAL,
    genre_names TEXT,
    enriched_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_channels_playlist_id ON channels(playlist_id);
CREATE INDEX IF NOT EXISTS idx_channels_group_title ON channels(playlist_id, group_title);
CREATE INDEX IF NOT EXISTS idx_channels_name ON channels(name);
-- idx_channels_tmdb_id is created by migrate_tmdb_columns(), because an older
-- database has no tmdb_id column until that ALTER runs and SCHEMA_SQL runs first.

CREATE TABLE IF NOT EXISTS profiles (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    color TEXT,
    pin_hash TEXT,
    created_at INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS favorites (
    profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    PRIMARY KEY (profile_id, channel_id)
);

CREATE TABLE IF NOT EXISTS history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    profile_id INTEGER NOT NULL DEFAULT 1,
    channel_id INTEGER REFERENCES channels(id) ON DELETE SET NULL,
    path TEXT NOT NULL,
    position REAL NOT NULL,
    duration REAL NOT NULL,
    played_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"""

STARTER_CHANNELS = [
    (
        "Red Bull TV HD",
        "https://rbmn-live.akamaized.net/hls/live/590964/BoRB-AT/master.m3u8",
        "Sports",
        "redbull.tv",
        "",
    ),
    (
        "DW English Live (International News)",
        "https://dwamdstream102.akamaized.net/hls/live/2015525/dwstream102/index.m3u8",
        "News",
        "dw.en",
        "",
    ),
    (
        "Big Buck Bunny (Open Movie HLS)",
        "https://test-streams.mux.dev/x36xhzz/x36xhzz.m3u8",
        "Movies",
        "bbb.mux",
        "",
    ),
    (
        "Tears of Steel (Blender Open Movie)",
        "https://demo.unified-streaming.com/k8s/features/stable/video/tears-of-steel/tears-of-steel.ism/.m3u8",
        "Movies",
        "tos.blender",
        "",
    ),
    (
        "Apple Advanced BipBop (fMP4 HLS)",
        "https://devstreaming-cdn.apple.com/videos/streaming/examples/img_bipbop_adv_example_fmp4/master.m3u8",
        "Demo Streams",
        "apple.bipbop",
        "",
    ),
]


@contextmanager
def get_conn():
    conn = sqlite3.connect(str(DB_PATH))
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA foreign_keys = ON;")
    try:
        with conn:
            yield conn
    finally:
        conn.close()


def channel_exists(conn: sqlite3.Connection, channel_id: Any) -> bool:
    """Return True only when `channel_id` maps to a real row in `channels`."""
    if channel_id is None:
        return False
    try:
        cid = int(channel_id)
    except (TypeError, ValueError):
        return False
    return conn.execute("SELECT 1 FROM channels WHERE id = ?", (cid,)).fetchone() is not None


def migrate_tmdb_columns(conn: sqlite3.Connection) -> None:
    """Add the TMDB enrichment columns to a database created before they existed.

    `CREATE TABLE IF NOT EXISTS` never alters a table that already exists, so an
    existing library needs each missing column added explicitly. On a fresh
    database every column is already present and this is a no-op.
    """
    existing = {row["name"] for row in conn.execute("PRAGMA table_info(channels)")}
    for name, kind in tmdb.TMDB_COLUMNS:
        if name not in existing:
            conn.execute(f"ALTER TABLE channels ADD COLUMN {name} {kind}")
    if "language" not in existing:
        conn.execute("ALTER TABLE channels ADD COLUMN language TEXT")
    conn.execute("CREATE INDEX IF NOT EXISTS idx_channels_tmdb_id ON channels(tmdb_id)")


def _set_tmdb_key(key: str) -> None:
    global _TMDB_KEY
    _TMDB_KEY = key


def _drop_legacy_tmdb_row(conn: sqlite3.Connection) -> None:
    """Move a plaintext TMDB key from settings into memory, then delete the row."""
    row = conn.execute("SELECT value FROM settings WHERE key = ?", (_TMDB_KEY_SETTING,)).fetchone()
    if row is None:
        return
    legacy = (row["value"] or "").strip()
    if legacy:
        _set_tmdb_key(legacy)
    conn.execute("DELETE FROM settings WHERE key = ?", (_TMDB_KEY_SETTING,))
    conn.commit()


def resolve_tmdb_key(conn: sqlite3.Connection) -> tuple[str, str]:
    """Return ``(api_key, source)``. The environment wins over the in-memory key."""
    _drop_legacy_tmdb_row(conn)
    env_key = (os.environ.get("LUMEN_TMDB_API_KEY") or "").strip()
    if env_key:
        return env_key, "env"
    if _TMDB_KEY:
        return _TMDB_KEY, "memory"
    return "", "none"


def repair_orphan_rows(conn: sqlite3.Connection) -> None:
    """Null out history/favorite rows that point at channels which no longer exist.

    Rows can become orphaned when channels are deleted by a connection that did not
    enable `PRAGMA foreign_keys`, which skips ON DELETE SET NULL / CASCADE.
    """
    conn.execute(
        "UPDATE history SET channel_id = NULL "
        "WHERE channel_id IS NOT NULL "
        "AND channel_id NOT IN (SELECT id FROM channels)"
    )
    conn.execute(
        "DELETE FROM favorites WHERE channel_id NOT IN (SELECT id FROM channels)"
    )


# The id the desktop app's first migration gives the profile that owns existing data.
DEFAULT_PROFILE_ID = 1
ACTIVE_PROFILE_KEY = "active_profile_id"
MAX_PROFILE_NAME_CHARS = 40
PROFILE_COLUMNS = "id, name, color, pin_hash IS NOT NULL AS has_pin, created_at, last_used_at"


def migrate_profiles(conn: sqlite3.Connection) -> None:
    """Bring a database up to the profile schema the desktop app uses.

    Fresh databases get the new tables from SCHEMA_SQL. A database made by an older bridge
    still has the single-profile `history` and `favorites` tables, so their rows are adopted
    by the first profile, as the desktop migration does. Every step is a no-op once applied.
    """
    if conn.execute("SELECT 1 FROM profiles LIMIT 1").fetchone() is None:
        now = int(time.time())
        conn.execute(
            "INSERT INTO profiles (id, name, color, pin_hash, created_at, last_used_at) "
            "VALUES (?, 'Lumen', 'accent', NULL, ?, ?)",
            (DEFAULT_PROFILE_ID, now, now),
        )

    history_cols = {row["name"] for row in conn.execute("PRAGMA table_info(history)")}
    if "profile_id" not in history_cols:
        conn.execute(f"ALTER TABLE history ADD COLUMN profile_id INTEGER NOT NULL DEFAULT {DEFAULT_PROFILE_ID}")

    favorite_cols = {row["name"] for row in conn.execute("PRAGMA table_info(favorites)")}
    if "profile_id" not in favorite_cols:
        conn.execute(
            "CREATE TABLE favorites_new ("
            "profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE, "
            "channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE, "
            "PRIMARY KEY (profile_id, channel_id))"
        )
        conn.execute(
            f"INSERT INTO favorites_new (profile_id, channel_id) SELECT {DEFAULT_PROFILE_ID}, channel_id FROM favorites"
        )
        conn.execute("DROP TABLE favorites")
        conn.execute("ALTER TABLE favorites_new RENAME TO favorites")

    conn.execute("CREATE INDEX IF NOT EXISTS idx_history_profile_played_at ON history(profile_id, played_at)")
    conn.commit()


def _profile_row(conn: sqlite3.Connection, profile_id: int) -> sqlite3.Row | None:
    return conn.execute(f"SELECT {PROFILE_COLUMNS} FROM profiles WHERE id = ?", (profile_id,)).fetchone()


def _profile_json(row: sqlite3.Row) -> Dict[str, Any]:
    data = dict(row)
    data["has_pin"] = bool(data["has_pin"])
    return data


def _active_profile_id(conn: sqlite3.Connection) -> int:
    """The profile the bridge acts as. Falls back to the oldest profile if the stored one is gone."""
    row = conn.execute("SELECT value FROM settings WHERE key = ?", (ACTIVE_PROFILE_KEY,)).fetchone()
    if row is not None:
        try:
            stored = int(row["value"])
        except (TypeError, ValueError):
            stored = None
        if stored is not None and _profile_row(conn, stored) is not None:
            return stored
    first = conn.execute("SELECT id FROM profiles ORDER BY created_at, id LIMIT 1").fetchone()
    return int(first["id"]) if first is not None else DEFAULT_PROFILE_ID


def _clean_profile_name(raw: Any) -> str:
    name = str(raw or "").strip()
    if not name:
        raise HTTPException(status_code=400, detail="A profile needs a name")
    if len(name) > MAX_PROFILE_NAME_CHARS:
        raise HTTPException(
            status_code=400,
            detail=f"Profile names are limited to {MAX_PROFILE_NAME_CHARS} characters",
        )
    return name


def _profile_id_from(body: Dict[str, Any]) -> int:
    raw = body.get("profileId", body.get("profile_id"))
    try:
        return int(raw)
    except (TypeError, ValueError):
        raise HTTPException(status_code=400, detail="A profile id is required")


def _list_profiles(conn: sqlite3.Connection) -> List[Dict[str, Any]]:
    rows = conn.execute(f"SELECT {PROFILE_COLUMNS} FROM profiles ORDER BY created_at, id").fetchall()
    return [_profile_json(r) for r in rows]


def init_db() -> None:
    with get_conn() as conn:
        conn.executescript(SCHEMA_SQL)
        migrate_profiles(conn)
        migrate_tmdb_columns(conn)
        repair_orphan_rows(conn)
        cur = conn.execute("SELECT COUNT(*) AS cnt FROM playlists")
        count = cur.fetchone()["cnt"]
        if count == 0 and os.environ.get("LUMEN_SEED_CATALOG", "1") == "1":
            now = int(time.time())
            cur = conn.execute(
                "INSERT INTO playlists (name, source, origin, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
                ("Public FTA & Open Streams", "catalog", "built-in-fta", now, now),
            )
            pid = cur.lastrowid
            conn.executemany(
                """
                INSERT INTO channels (playlist_id, name, url, group_title, tvg_id, tvg_logo, extra_headers, last_ok)
                VALUES (?, ?, ?, ?, ?, ?, '{}', ?)
                """,
                [(pid, name, url, grp, tid, logo, None) for (name, url, grp, tid, logo) in STARTER_CHANNELS],
            )
            conn.commit()


init_db()


LOCAL_VIDEO_EXTENSIONS = {".mp4", ".mkv", ".avi", ".mov", ".webm", ".ts", ".m2ts"}
LOCAL_MEDIA_EXTENSIONS = {
    ".mp4", ".mkv", ".avi", ".mov", ".webm", ".ts", ".m2ts",
    ".mp3", ".flac", ".wav", ".ogg", ".opus", ".m4a", ".m3u", ".m3u8"
}


def _local_media_roots() -> list[Path]:
    """Directories the browser bridge may read from: the home directory plus any
    directories listed in LUMEN_LOCAL_MEDIA_ROOTS (separated by os.pathsep)."""
    candidates = [Path.home()]
    extra = os.environ.get("LUMEN_LOCAL_MEDIA_ROOTS", "")
    candidates.extend(Path(item).expanduser() for item in extra.split(os.pathsep) if item.strip())
    roots: list[Path] = []
    for candidate in candidates:
        try:
            roots.append(candidate.resolve(strict=True))
        except (OSError, RuntimeError):
            continue
    return roots


def _authorized_local_file(raw_path: str, extensions: set[str], not_found_detail: str) -> Path:
    """Resolve a browser-supplied path to a media file inside an allowed root.

    Symlinks are resolved first, so a link inside an allowed root that points elsewhere
    is rejected. Paths outside the roots get the same 404 as missing files.
    """
    try:
        source = Path(raw_path).expanduser().resolve(strict=True)
    except (OSError, RuntimeError) as error:
        raise HTTPException(status_code=404, detail="Local file not found") from error
    if not source.is_file() or source.suffix.lower() not in extensions:
        raise HTTPException(status_code=404, detail=not_found_detail)
    if not any(source.is_relative_to(root) for root in _local_media_roots()):
        raise HTTPException(status_code=404, detail=not_found_detail)
    return source


POSTER_FRAME_SETTING = "poster_frame_seconds"
DEFAULT_POSTER_FRAME_SECONDS = 2
# Frame times offered in Settings. Only these reach ffmpeg, so a hand-edited row cannot seek arbitrarily.
POSTER_FRAME_CHOICES = (0, 2, 5, 10, 30, 60)


def _parse_poster_frame_seconds(raw: str | None) -> int | None:
    if raw is None:
        return None
    try:
        seconds = int(str(raw).strip())
    except ValueError:
        return None
    return seconds if seconds in POSTER_FRAME_CHOICES else None


def _current_poster_frame_seconds() -> int:
    with get_conn() as conn:
        row = conn.execute("SELECT value FROM settings WHERE key = ?", (POSTER_FRAME_SETTING,)).fetchone()
    parsed = _parse_poster_frame_seconds(row["value"] if row else None)
    return parsed if parsed is not None else DEFAULT_POSTER_FRAME_SECONDS


def _clear_thumbnail_cache() -> None:
    """Remove cached posters and filmstrips so they regenerate with the current settings."""
    cache_dir = DB_DIR / "thumbnails"
    if cache_dir.exists():
        shutil.rmtree(cache_dir)


def local_thumbnail_file(raw_path: str) -> Path:
    """Return a cached poster frame at the saved frame time, generating it on first request."""
    return local_frame_file(raw_path, _current_poster_frame_seconds(), fall_back_to_start=True, not_found_detail="No video poster for this file")


def local_frame_file(raw_path: str, frame_seconds: int, fall_back_to_start: bool, not_found_detail: str = "No video poster for this file") -> Path:
    """Return a cached video frame at frame_seconds, generating it with ffmpeg on first request.

    The frame cache is keyed by canonical path, file size, modification time and the
    chosen frame time so an updated movie or a changed frame setting gets a fresh poster
    without rescanning or decoding it at page load. Previews pass fall_back_to_start=False
    so a frame past the end of a short clip fails instead of silently showing the first frame.
    """
    source = _authorized_local_file(raw_path, LOCAL_VIDEO_EXTENSIONS, not_found_detail)

    try:
        stat = source.stat()
    except OSError as error:
        raise HTTPException(status_code=404, detail="Local video not found") from error
    fingerprint = f"{source}\0{stat.st_size}\0{stat.st_mtime_ns}\0{frame_seconds}".encode("utf-8")
    key = hashlib.sha256(fingerprint).hexdigest()
    cache_dir = DB_DIR / "thumbnails"
    cache_dir.mkdir(parents=True, exist_ok=True)
    cached = cache_dir / f"{key}.jpg"
    try:
        if cached.is_file() and cached.stat().st_size > 4 and cached.read_bytes()[:2] == b"\xff\xd8":
            return cached
    except OSError:
        pass

    executable = shutil.which("ffmpeg")
    if executable is None:
        raise HTTPException(status_code=503, detail="Video thumbnails require ffmpeg")

    temporary = cache_dir / f".{key}.{uuid.uuid4().hex}.jpg"
    base_command = [
        executable, "-nostdin", "-hide_banner", "-loglevel", "error",
        "-i", str(source), "-map", "0:v:0", "-frames:v", "1",
        "-vf", "scale=480:-2:force_original_aspect_ratio=decrease",
        "-q:v", "5", "-threads", "1", "-an", "-sn", "-dn", "-y",
        str(temporary),
    ]
    try:
        # The chosen frame skips intros; retry at the first frame for clips shorter than that.
        seeks = [str(frame_seconds)]
        if fall_back_to_start and frame_seconds:
            seeks.append("0")
        for seek in seeks:
            command = [*base_command[:5], "-ss", seek, *base_command[5:]]
            try:
                result = subprocess.run(command, check=False, capture_output=True, timeout=12)
            except subprocess.TimeoutExpired as error:
                raise HTTPException(status_code=504, detail="Video thumbnail generation timed out") from error
            except OSError as error:
                raise HTTPException(status_code=503, detail="Could not start ffmpeg") from error
            if result.returncode == 0 and temporary.is_file():
                try:
                    if temporary.stat().st_size <= 2_000_000 and temporary.read_bytes()[:2] == b"\xff\xd8":
                        os.replace(temporary, cached)
                        return cached
                except OSError:
                    pass
            temporary.unlink(missing_ok=True)
        raise HTTPException(status_code=422, detail="Could not extract a frame from this video")
    finally:
        temporary.unlink(missing_ok=True)


def _poster_candidates(raw_path: str) -> List[Dict[str, Any]]:
    """Render each offered frame time so the user can compare them before saving one.

    The first frame must succeed, since that proves the file is a readable video. Later
    frames that run past the end of a short clip come back as null instead of failing the list.
    """
    candidates: List[Dict[str, Any]] = []
    for index, seconds in enumerate(POSTER_FRAME_CHOICES):
        try:
            cached = local_frame_file(raw_path, seconds, fall_back_to_start=False, not_found_detail="No video preview for this file")
        except HTTPException:
            if index == 0:
                raise
            candidates.append({"seconds": seconds, "image": None})
            continue
        candidates.append({"seconds": seconds, "image": base64.b64encode(cached.read_bytes()).decode("ascii")})
    return candidates


def local_filmstrip_file(raw_path: str) -> Path:
    """Return a cached four-frame JPEG strip for an on-demand hover preview."""
    source = _authorized_local_file(raw_path, LOCAL_VIDEO_EXTENSIONS, "No video preview for this file")

    try:
        stat = source.stat()
    except OSError as error:
        raise HTTPException(status_code=404, detail="Local video not found") from error
    fingerprint = f"{source}\0{stat.st_size}\0{stat.st_mtime_ns}\0filmstrip".encode("utf-8")
    key = hashlib.sha256(fingerprint).hexdigest()
    cache_dir = DB_DIR / "thumbnails"
    cache_dir.mkdir(parents=True, exist_ok=True)
    cached = cache_dir / f"{key}.jpg"
    try:
        if cached.is_file() and cached.stat().st_size > 4 and cached.read_bytes()[:2] == b"\xff\xd8":
            return cached
    except OSError:
        pass

    ffmpeg = shutil.which("ffmpeg")
    ffprobe = shutil.which("ffprobe")
    if ffmpeg is None or ffprobe is None:
        raise HTTPException(status_code=503, detail="Video previews require ffmpeg and ffprobe")
    try:
        probe = subprocess.run(
            [ffprobe, "-v", "error", "-show_entries", "format=duration", "-of", "default=noprint_wrappers=1:nokey=1", str(source)],
            check=False, capture_output=True, text=True, timeout=8,
        )
        duration = float(probe.stdout.strip()) if probe.returncode == 0 else 0.0
    except (OSError, subprocess.TimeoutExpired, ValueError):
        duration = 0.0
    if not (duration > 0):
        raise HTTPException(status_code=422, detail="Could not determine video duration for preview")

    temporary = cache_dir / f".{key}.{uuid.uuid4().hex}.jpg"
    command = [ffmpeg, "-nostdin", "-hide_banner", "-loglevel", "error"]
    for fraction in (0.12, 0.36, 0.62, 0.88):
        command.extend(["-ss", f"{duration * fraction:.3f}", "-i", str(source)])
    filters = []
    for index in range(4):
        filters.append(f"[{index}:v:0]scale=240:136:force_original_aspect_ratio=increase,crop=240:136,setsar=1[v{index}]")
    filters.append("[v0][v1][v2][v3]hstack=inputs=4[out]")
    command.extend([
        "-filter_complex", ";".join(filters), "-map", "[out]", "-frames:v", "1",
        "-q:v", "6", "-threads", "1", "-an", "-sn", "-dn", "-y", str(temporary),
    ])
    try:
        try:
            result = subprocess.run(command, check=False, capture_output=True, timeout=18)
        except subprocess.TimeoutExpired as error:
            raise HTTPException(status_code=504, detail="Video preview generation timed out") from error
        except OSError as error:
            raise HTTPException(status_code=503, detail="Could not start ffmpeg") from error
        if result.returncode == 0 and temporary.is_file():
            try:
                if temporary.stat().st_size <= 2_000_000 and temporary.read_bytes()[:2] == b"\xff\xd8":
                    os.replace(temporary, cached)
                    return cached
            except OSError:
                pass
        raise HTTPException(status_code=422, detail="Could not extract preview frames from this video")
    finally:
        temporary.unlink(missing_ok=True)


ATTR_RE = re.compile(r'([A-Za-z0-9_-]+)="([^"]*)"')


def _split_extinf(body: str) -> tuple[str, str]:
    in_quotes = False
    for idx, ch in enumerate(body):
        if ch == '"':
            in_quotes = not in_quotes
        elif ch == "," and not in_quotes:
            return body[:idx], body[idx + 1 :]
    return body, ""


def parse_m3u(content: str, playlist_id: int) -> List[Dict[str, Any]]:
    channels: List[Dict[str, Any]] = []
    current_extinf: Optional[Dict[str, Any]] = None
    current_extgrp: Optional[str] = None
    pending_headers: Dict[str, str] = {}

    for raw_line in content.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#EXTM3U"):
            continue
        if line.startswith("#EXTINF:"):
            body = line[len("#EXTINF:") :]
            attr_part, name_part = _split_extinf(body)
            attrs = {k.lower(): v for k, v in ATTR_RE.findall(attr_part)}
            current_extinf = {"attrs": attrs, "name": name_part.strip()}
        elif line.startswith("#EXTGRP:"):
            grp = line[len("#EXTGRP:") :].strip()
            if grp:
                current_extgrp = grp
        elif line.startswith("#EXTVLCOPT:"):
            opt = line[len("#EXTVLCOPT:") :].strip()
            if "=" in opt:
                k, v = opt.split("=", 1)
                kl = k.strip().lower()
                val = v.strip()
                if kl == "http-user-agent":
                    pending_headers["User-Agent"] = val
                elif kl in ("http-referrer", "http-referer"):
                    pending_headers["Referer"] = val
                elif kl == "http-origin":
                    pending_headers["Origin"] = val
                else:
                    pending_headers[k.strip()] = val
        elif line.startswith("#"):
            continue
        else:
            url = line
            if current_extinf:
                attrs = current_extinf["attrs"]
                name = current_extinf["name"] or attrs.get("tvg-name") or url
                group_title = attrs.get("group-title") or current_extgrp
                tvg_id = attrs.get("tvg-id")
                tvg_logo = attrs.get("tvg-logo")
                language = (attrs.get("tvg-language") or "").strip() or None
                current_extinf = None
            else:
                name = url.rsplit("/", 1)[-1].split("?")[0] or url
                group_title = current_extgrp
                tvg_id = None
                tvg_logo = None
                language = None

            extra_headers = json.dumps(pending_headers) if pending_headers else "{}"
            pending_headers.clear()

            channels.append(
                {
                    "playlist_id": playlist_id,
                    "name": name,
                    "url": url,
                    "group_title": group_title,
                    "tvg_id": tvg_id,
                    "tvg_logo": tvg_logo,
                    "language": language,
                    "extra_headers": extra_headers,
                    "last_ok": None,
                    "last_error": None,
                }
            )
    return channels


class MpvController:
    def __init__(self) -> None:
        self.proc: Optional[subprocess.Popen] = None
        self.sock_path = f"/tmp/lumen-mpv-{os.getpid()}.sock"

    def _send_ipc(self, command: list) -> Any:
        import socket as pysock

        if not os.path.exists(self.sock_path):
            raise HTTPException(status_code=503, detail="No active mpv player")
        try:
            with pysock.socket(pysock.AF_UNIX, pysock.SOCK_STREAM) as s:
                s.settimeout(2.0)
                s.connect(self.sock_path)
                payload = json.dumps({"command": command, "request_id": 1}) + "\n"
                s.sendall(payload.encode("utf-8"))
                with s.makefile("r") as stream:
                    for line in stream:
                        response = json.loads(line)
                        if response.get("request_id") == 1:
                            if response.get("error") != "success":
                                raise HTTPException(status_code=502, detail=f"mpv: {response.get('error')}")
                            return response.get("data")
                raise HTTPException(status_code=502, detail="mpv closed its IPC connection")
        except HTTPException:
            raise
        except Exception as error:
            raise HTTPException(status_code=502, detail="mpv IPC connection failed") from error

    def ensure_started(self, hwdec: str = "auto-safe", cache: str = "yes", extra_args: str = "") -> bool:
        if self.proc is not None and self.proc.poll() is None and os.path.exists(self.sock_path):
            return True
        try:
            if os.path.exists(self.sock_path):
                os.unlink(self.sock_path)
        except OSError:
            pass

        cmd = [
            "mpv",
            f"--input-ipc-server={self.sock_path}",
            "--idle=yes",
            "--force-window=yes",
            f"--hwdec={hwdec or 'auto-safe'}",
            "--vo=gpu-next",
            "--keep-open=yes",
            f"--cache={cache or 'yes'}",
            "--really-quiet",
        ]
        if extra_args.strip():
            cmd.extend(extra_args.split())

        try:
            self.proc = subprocess.Popen(
                cmd,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            for _ in range(30):
                if os.path.exists(self.sock_path):
                    return True
                time.sleep(0.05)
        except Exception:
            return False
        return False

    def load(self, url: str, headers: Optional[Dict[str, str]] = None, hwdec: str = "auto-safe", cache: str = "yes", extra_args: str = "") -> None:
        if not self.ensure_started(hwdec, cache, extra_args):
            raise HTTPException(status_code=503, detail="Could not start mpv. Check that mpv and a display session are available.")
        hdr_list = [f"{k}: {v}" for k, v in (headers or {}).items()]
        self._send_ipc(["set_property", "http-header-fields", hdr_list])
        self._send_ipc(["loadfile", url, "replace"])
        self._send_ipc(["set_property", "pause", False])

    def pause(self) -> None:
        self._send_ipc(["cycle", "pause"])

    def seek(self, seconds: float) -> None:
        self._send_ipc(["seek", max(0.0, seconds), "absolute"])

    def set_volume(self, vol: int) -> None:
        self._send_ipc(["set_property", "volume", max(0, min(150, vol))])

    def state(self) -> dict:
        idle = {"paused": True, "time": 0, "duration": 0, "buffering": False}
        if self.proc is None or self.proc.poll() is not None:
            return idle
        values = self._send_ipc(["get_property", "property-list"])
        result = dict(idle)
        for key, prop in [("paused", "pause"), ("time", "time-pos"), ("duration", "duration"), ("buffering", "paused-for-cache"), ("title", "media-title")]:
            if prop in values:
                try:
                    value = self._send_ipc(["get_property", prop])
                    if value is not None:
                        result[key] = value
                except HTTPException as error:
                    if "property unavailable" not in str(error.detail):
                        raise
        return result

    def stop(self) -> None:
        if self.proc is not None:
            try:
                self.proc.terminate()
            except Exception:
                pass
            self.proc = None


mpv_controller = MpvController()


def _search_library(conn: sqlite3.Connection, query: str) -> list[dict[str, Any]]:
    terms = [term for term in re.split(r"\s+", query.strip()) if term]
    sql = (
        "SELECT id, playlist_id, name, url, group_title, tvg_id, language, extra_headers, "
        "tmdb_type, release_year FROM channels"
    )
    params: list[Any] = []
    if terms:
        sql += " WHERE " + " AND ".join("(name LIKE ? OR group_title LIKE ? OR tvg_id LIKE ?)" for _ in terms)
        for term in terms:
            pattern = f"%{term}%"
            params.extend((pattern, pattern, pattern))
    sql += " ORDER BY name LIMIT 5000"
    return [dict(row) for row in conn.execute(sql, params).fetchall()]


@router.post("/invoke/{cmd}")
async def invoke_bridge(cmd: str, request: Request) -> Any:
    try:
        body = await request.json()
    except Exception:
        body = {}

    if cmd == "get_player_state":
        return await asyncio.to_thread(mpv_controller.state)

    if cmd == "list_playlists_cmd":
        with get_conn() as conn:
            rows = conn.execute(
                "SELECT id, name, source, origin, created_at, updated_at FROM playlists ORDER BY created_at DESC"
            ).fetchall()
            return [dict(r) for r in rows]

    elif cmd == "search_engine":
        query = str(body.get("query", "")).strip()
        if len(query) < 2:
            return {"query": query, "results": [], "providers": []}
        with get_conn() as conn:
            library = _search_library(conn, query)
            providers = _with_memory_tokens(_stored_providers(conn))
        try:
            return await search_engine.search(
                query,
                library,
                providers,
                check_availability=bool(body.get("checkAvailability", True)),
                preferred_language=body.get("preferredLanguage"),
            )
        except (ValueError, json.JSONDecodeError) as error:
            raise HTTPException(status_code=400, detail=str(error)) from error

    elif cmd == "list_channels_cmd":
        playlist_id = body.get("playlistId") or body.get("playlist_id")
        query = body.get("query")
        group = body.get("group")
        sql = (
            "SELECT id, playlist_id, name, url, group_title, tvg_id, tvg_logo, language, extra_headers, "
            "last_ok, last_error, tmdb_id, tmdb_type, poster_path, backdrop_path, overview, "
            "release_year, rating, genre_names, enriched_at "
            "FROM channels WHERE playlist_id = ?"
        )
        params: list[Any] = [playlist_id]
        if query:
            sql += " AND name LIKE ?"
            params.append(f"%{query}%")
        if group:
            sql += " AND group_title = ?"
            params.append(group)
        sql += " ORDER BY group_title, name"
        with get_conn() as conn:
            rows = conn.execute(sql, params).fetchall()
            return [dict(r) for r in rows]

    elif cmd == "import_playlist_file":
        path_str = body.get("path", "")
        p = Path(path_str)
        if not p.exists() or not p.is_file():
            raise HTTPException(status_code=400, detail=f"File not found: {path_str}")
        content = p.read_text(encoding="utf-8", errors="replace")
        now = int(time.time())
        with get_conn() as conn:
            cur = conn.execute(
                "INSERT INTO playlists (name, source, origin, created_at, updated_at) VALUES (?, 'file', ?, ?, ?)",
                (p.stem or "Imported Playlist", path_str, now, now),
            )
            pid = int(cur.lastrowid)
            channels = parse_m3u(content, pid)
            conn.executemany(
                """
                INSERT INTO channels (playlist_id, name, url, group_title, tvg_id, tvg_logo, language, extra_headers, last_ok, last_error)
                VALUES (:playlist_id, :name, :url, :group_title, :tvg_id, :tvg_logo, :language, :extra_headers, :last_ok, :last_error)
                """,
                channels,
            )
            conn.commit()
            return pid

    elif cmd == "import_playlist_url":
        from .extract import assert_public_request, ensure_public_http_url
        try:
            url = ensure_public_http_url(body.get("url", ""))
            async with httpx.AsyncClient(
                timeout=30.0,
                follow_redirects=True,
                event_hooks={"request": [assert_public_request]},
            ) as client:
                async with client.stream("GET", url) as resp:
                    resp.raise_for_status()
                    content = await read_capped_text(resp)
        except ValueError as error:
            raise HTTPException(status_code=400, detail=str(error))
        name = body.get("name") or (url.rsplit("/", 1)[-1].split("?")[0] or "Imported Playlist")
        now = int(time.time())
        with get_conn() as conn:
            cur = conn.execute(
                "INSERT INTO playlists (name, source, origin, created_at, updated_at) VALUES (?, 'url', ?, ?, ?)",
                (name, url, now, now),
            )
            pid = int(cur.lastrowid)
            channels = parse_m3u(content, pid)
            conn.executemany(
                """
                INSERT INTO channels (playlist_id, name, url, group_title, tvg_id, tvg_logo, language, extra_headers, last_ok, last_error)
                VALUES (:playlist_id, :name, :url, :group_title, :tvg_id, :tvg_logo, :language, :extra_headers, :last_ok, :last_error)
                """,
                channels,
            )
            conn.commit()
            return pid

    elif cmd == "delete_playlist_cmd":
        pid = body.get("playlistId") or body.get("playlist_id")
        with get_conn() as conn:
            conn.execute("DELETE FROM playlists WHERE id = ?", (pid,))
            conn.commit()
        return None

    elif cmd == "extract_url":
        url = body.get("url", "").strip()
        extracted = await extract_stream(url)
        now = int(time.time())
        with get_conn() as conn:
            cur = conn.execute(
                "INSERT INTO playlists (name, source, origin, created_at, updated_at) VALUES (?, 'extract', ?, ?, ?)",
                (extracted["title"], url, now, now),
            )
            pid = int(cur.lastrowid)
            conn.execute(
                """
                INSERT INTO channels (playlist_id, name, url, group_title, extra_headers, last_ok)
                VALUES (?, ?, ?, 'Extracted', ?, ?)
                """,
                (pid, extracted["title"], extracted["url"], json.dumps(extracted.get("headers") or {}), now),
            )
            conn.commit()
        return extracted

    elif cmd == "harvest_page":
        url = body.get("url", "").strip()
        urls = await harvest_streams(url)
        if urls:
            now = int(time.time())
            host = urlparse(url).hostname or "Harvested Page"
            with get_conn() as conn:
                cur = conn.execute(
                    "INSERT INTO playlists (name, source, origin, created_at, updated_at) VALUES (?, 'extract', ?, ?, ?)",
                    (f"Harvested: {host}", url, now, now),
                )
                pid = int(cur.lastrowid)
                for idx, u in enumerate(urls):
                    sname = u.rsplit("/", 1)[-1].split("?")[0] or f"Stream #{idx + 1}"
                    conn.execute(
                        """
                        INSERT INTO channels (playlist_id, name, url, group_title, extra_headers)
                        VALUES (?, ?, ?, 'Harvested', '{}')
                        """,
                        (pid, sname, u),
                    )
                conn.commit()
        return {"urls": urls}

    elif cmd == "probe_urls":
        ids: List[int] = body.get("ids") or []
        with get_conn() as conn:
            id_to_url: List[tuple[int, str]] = []
            for cid in ids:
                row = conn.execute("SELECT url FROM channels WHERE id = ?", (cid,)).fetchone()
                if row:
                    id_to_url.append((cid, row["url"]))
        results = await probe_urls([u for _, u in id_to_url])
        url_map = {r["url"]: r for r in results}
        now = int(time.time())
        with get_conn() as conn:
            for cid, u in id_to_url:
                r = url_map.get(u)
                if r:
                    if r["live"]:
                        conn.execute(
                            "UPDATE channels SET last_ok = ?, last_error = NULL WHERE id = ?",
                            (now, cid),
                        )
                    else:
                        err = f"HTTP {r['status']}" if r.get("status") else "Unreachable"
                        conn.execute(
                            "UPDATE channels SET last_ok = NULL, last_error = ? WHERE id = ?",
                            (err, cid),
                        )
            conn.commit()
        return {"results": results}

    elif cmd == "list_profiles_cmd":
        with get_conn() as conn:
            return _list_profiles(conn)

    elif cmd == "active_profile_cmd":
        with get_conn() as conn:
            return _profile_json(_profile_row(conn, _active_profile_id(conn)))

    elif cmd == "create_profile_cmd":
        name = _clean_profile_name(body.get("name"))
        color = str(body["color"]) if body.get("color") else None
        now = int(time.time())
        with get_conn() as conn:
            cur = conn.execute(
                "INSERT INTO profiles (name, color, pin_hash, created_at, last_used_at) VALUES (?, ?, NULL, ?, ?)",
                (name, color, now, now),
            )
            conn.commit()
            return _profile_json(_profile_row(conn, cur.lastrowid))

    elif cmd == "rename_profile_cmd":
        pid = _profile_id_from(body)
        name = _clean_profile_name(body.get("name"))
        with get_conn() as conn:
            cur = conn.execute("UPDATE profiles SET name = ? WHERE id = ?", (name, pid))
            if cur.rowcount == 0:
                raise HTTPException(status_code=404, detail="Profile no longer exists")
            conn.commit()
            return _profile_json(_profile_row(conn, pid))

    elif cmd == "delete_profile_cmd":
        pid = _profile_id_from(body)
        with get_conn() as conn:
            total = conn.execute("SELECT COUNT(*) AS total FROM profiles").fetchone()["total"]
            if total <= 1:
                raise HTTPException(status_code=400, detail="The only profile cannot be deleted")
            if _profile_row(conn, pid) is None:
                raise HTTPException(status_code=404, detail="Profile no longer exists")
            # Explicit deletes: history rows added by an upgrade carry no foreign key to cascade through.
            conn.execute("DELETE FROM history WHERE profile_id = ?", (pid,))
            conn.execute("DELETE FROM favorites WHERE profile_id = ?", (pid,))
            conn.execute("DELETE FROM profiles WHERE id = ?", (pid,))
            conn.commit()
            return _list_profiles(conn)

    elif cmd == "set_profile_pin_cmd":
        pid = _profile_id_from(body)
        pin = body.get("pin")
        with get_conn() as conn:
            if _profile_row(conn, pid) is None:
                raise HTTPException(status_code=404, detail="Profile no longer exists")
            # Argon2 PIN hashes are created and verified by the desktop app. The browser bridge can
            # clear a PIN, as the desktop command does, but it cannot set one it could not check later.
            if pin:
                raise HTTPException(
                    status_code=400,
                    detail="PINs are set in the Lumen desktop app",
                )
            conn.execute("UPDATE profiles SET pin_hash = NULL WHERE id = ?", (pid,))
            conn.commit()
            return _profile_json(_profile_row(conn, pid))

    elif cmd == "activate_profile_cmd":
        pid = _profile_id_from(body)
        with get_conn() as conn:
            row = _profile_row(conn, pid)
            if row is None:
                raise HTTPException(status_code=404, detail="Profile no longer exists")
            # The browser bridge cannot verify a PIN, so a locked profile stays locked here.
            if row["has_pin"]:
                raise HTTPException(
                    status_code=403,
                    detail="This profile is locked. Unlock it in the Lumen desktop app.",
                )
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?, ?) "
                "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                (ACTIVE_PROFILE_KEY, str(pid)),
            )
            conn.execute("UPDATE profiles SET last_used_at = ? WHERE id = ?", (int(time.time()), pid))
            conn.commit()
            return _profile_json(_profile_row(conn, pid))

    elif cmd == "toggle_favorite_cmd":
        cid = body.get("channelId") or body.get("channel_id")
        with get_conn() as conn:
            if not channel_exists(conn, cid):
                raise HTTPException(status_code=404, detail="Channel no longer exists")
            pid = _active_profile_id(conn)
            exists = conn.execute(
                "SELECT 1 FROM favorites WHERE profile_id = ? AND channel_id = ?", (pid, cid)
            ).fetchone()
            if exists:
                conn.execute("DELETE FROM favorites WHERE profile_id = ? AND channel_id = ?", (pid, cid))
                conn.commit()
                return False
            else:
                conn.execute("INSERT INTO favorites (profile_id, channel_id) VALUES (?, ?)", (pid, cid))
                conn.commit()
                return True

    elif cmd == "list_favorites_cmd":
        with get_conn() as conn:
            rows = conn.execute(
                "SELECT channel_id FROM favorites WHERE profile_id = ?", (_active_profile_id(conn),)
            ).fetchall()
            return [r["channel_id"] for r in rows]

    elif cmd == "get_history_cmd":
        limit = body.get("limit") or 50
        with get_conn() as conn:
            rows = conn.execute(
                "SELECT id, channel_id, path, position, duration, played_at FROM history "
                "WHERE profile_id = ? ORDER BY played_at DESC LIMIT ?",
                (_active_profile_id(conn), limit),
            ).fetchall()
            return [dict(r) for r in rows]

    elif cmd == "save_progress_cmd":
        cid = body.get("channelId")
        path = body.get("path", "")
        pos = float(body.get("position") or 0.0)
        dur = float(body.get("duration") or 0.0)
        now = int(time.time())
        with get_conn() as conn:
            if cid is not None and not channel_exists(conn, cid):
                cid = None
            conn.execute(
                "INSERT INTO history (profile_id, channel_id, path, position, duration, played_at) VALUES (?, ?, ?, ?, ?, ?)",
                (_active_profile_id(conn), cid, path, pos, dur, now),
            )
            conn.commit()
        return None

    elif cmd == "list_local_dir":
        raw_path = (body.get("path") or "").strip()
        target = Path(raw_path) if raw_path else Path.home()
        if not target.exists() or not target.is_dir():
            target = Path.home()
        files = []
        for entry in target.iterdir():
            if entry.name.startswith("."):
                continue
            try:
                if entry.is_dir():
                    files.append({"name": entry.name, "path": str(entry), "is_dir": True, "size": None})
                elif entry.is_file() and entry.suffix.lower() in LOCAL_MEDIA_EXTENSIONS:
                    files.append(
                        {
                            "name": entry.name,
                            "path": str(entry),
                            "is_dir": False,
                            "size": entry.stat().st_size,
                        }
                    )
            except OSError:
                continue
        files.sort(key=lambda x: (not x["is_dir"], x["name"].lower()))
        return files

    elif cmd == "get_search_providers":
        with get_conn() as conn:
            return _stored_providers(conn)

    elif cmd == "set_search_providers":
        providers = body.get("providers") or []
        if not isinstance(providers, list) or len(providers) > 12:
            raise HTTPException(status_code=400, detail="At most 12 external providers can be enabled")
        try:
            async with httpx.AsyncClient(follow_redirects=False) as client:
                from .search_engine import ProviderManager
                ProviderManager(client).build([], providers)
        except (TypeError, ValueError) as error:
            raise HTTPException(status_code=400, detail=f"Invalid search providers: {error}") from error
        return _save_search_providers(providers)

    elif cmd == "get_settings":
        with get_conn() as conn:
            rows = conn.execute("SELECT key, value FROM settings").fetchall()
            smap = {r["key"]: r["value"] for r in rows}
            return {
                "hwdec": smap.get("hwdec", "auto-safe"),
                "extra_mpv_args": smap.get("extra_mpv_args", ""),
                "cache": smap.get("cache", "yes"),
                "spawn_external_mpv": smap.get("spawn_external_mpv", "false"),
                "poster_frame_seconds": str(_current_poster_frame_seconds()),
                "search_preferred_language": smap.get("search_preferred_language", ""),
                "search_hide_other_languages": smap.get("search_hide_other_languages", ""),
            }

    elif cmd == "local_poster_candidates":
        return await asyncio.to_thread(_poster_candidates, str(body.get("path", "")))

    elif cmd == "set_setting_cmd":
        k = body.get("key", "")
        v = body.get("value", "")
        if k == "search_providers":
            # Provider tokens must go through set_search_providers, which keeps them out of the settings table.
            raise HTTPException(status_code=400, detail="Use set_search_providers to change search providers")
        if k == _TMDB_KEY_SETTING:
            raise HTTPException(status_code=400, detail="Use set_tmdb_api_key to change the TMDB API key")
        if k == POSTER_FRAME_SETTING:
            seconds = _parse_poster_frame_seconds(v)
            if seconds is None:
                raise HTTPException(status_code=400, detail="Poster frame must be one of the offered times")
            previous = _current_poster_frame_seconds()
            v = str(seconds)
            if previous != seconds:
                _clear_thumbnail_cache()
        with get_conn() as conn:
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = ?",
                (k, v, v),
            )
            conn.commit()
        return None

    elif cmd == "set_tmdb_api_key":
        key = body.get("key", "")
        if not isinstance(key, str):
            raise HTTPException(status_code=400, detail="TMDB API key must be a string")
        _set_tmdb_key(key.strip())
        with get_conn() as conn:
            conn.execute("DELETE FROM settings WHERE key = ?", (_TMDB_KEY_SETTING,))
            conn.commit()
        return None

    elif cmd == "play_url":
        url = body.get("url", "")
        headers = body.get("headers")
        cid = body.get("channelId")
        now = int(time.time())
        with get_conn() as conn:
            if cid is not None and not channel_exists(conn, cid):
                cid = None
            conn.execute(
                "INSERT INTO history (profile_id, channel_id, path, position, duration, played_at) VALUES (?, ?, ?, 0, 0, ?)",
                (_active_profile_id(conn), cid, url, now),
            )
            if cid:
                conn.execute("UPDATE channels SET last_ok = ?, last_error = NULL WHERE id = ?", (now, cid))
            rows = conn.execute("SELECT key, value FROM settings").fetchall()
            smap = {r["key"]: r["value"] for r in rows}
            conn.commit()

        # Embedded browser playback is the default; external mpv is opt-in.
        if smap.get("spawn_external_mpv", "false") == "true":
            await asyncio.to_thread(
                mpv_controller.load,
                url,
                headers,
                smap.get("hwdec", "auto-safe"),
                smap.get("cache", "yes"),
                smap.get("extra_mpv_args", ""),
            )
        return None

    elif cmd == "pause":
        await asyncio.to_thread(mpv_controller.pause)
        return None

    elif cmd == "seek":
        sec = float(body.get("seconds") or 0.0)
        await asyncio.to_thread(mpv_controller.seek, sec)
        return None

    elif cmd == "set_volume":
        vol = int(body.get("vol", 80))
        await asyncio.to_thread(mpv_controller.set_volume, vol)
        return None

    elif cmd == "stop":
        await asyncio.to_thread(mpv_controller.stop)
        return None

    elif cmd == "tmdb_status":
        with get_conn() as conn:
            api_key, source = resolve_tmdb_key(conn)
            enriched = conn.execute(
                "SELECT COUNT(*) AS c FROM channels WHERE tmdb_id IS NOT NULL"
            ).fetchone()["c"]
            total = conn.execute("SELECT COUNT(*) AS c FROM channels").fetchone()["c"]
        # The key value itself is never returned, only whether one is present.
        return {
            "configured": bool(api_key),
            "key_source": source,
            "enriched": enriched,
            "total": total,
            "image_base": tmdb.IMAGE_BASE,
            "poster_size": tmdb.POSTER_SIZE,
            "backdrop_size": tmdb.BACKDROP_SIZE,
        }

    elif cmd == "tmdb_enrich":
        limit = int(body.get("limit") or tmdb.DEFAULT_LIMIT)
        force = bool(body.get("force"))
        with get_conn() as conn:
            api_key, _ = resolve_tmdb_key(conn)
            if not api_key:
                raise HTTPException(status_code=400, detail="TMDB API key not configured")
            rows = conn.execute(
                "SELECT id, name FROM channels WHERE (enriched_at IS NULL OR ?) ORDER BY id LIMIT ?",
                (1 if force else 0, limit),
            ).fetchall()
            candidates = [(r["id"], r["name"]) for r in rows]

        def store(channel_id: int, updates: Dict[str, Any]) -> None:
            assignments = ", ".join(f"{column} = ?" for column in updates)
            with get_conn() as conn:
                conn.execute(
                    f"UPDATE channels SET {assignments} WHERE id = ?",
                    (*updates.values(), channel_id),
                )
                conn.commit()

        try:
            stats = await tmdb.enrich_channels(api_key, candidates, store)
        except tmdb.TmdbAuthError as error:
            raise HTTPException(status_code=400, detail=str(error)) from error

        with get_conn() as conn:
            remaining = conn.execute(
                "SELECT COUNT(*) AS c FROM channels WHERE enriched_at IS NULL"
            ).fetchone()["c"]
        return {**stats, "remaining": remaining}

    elif cmd == "tmdb_clear_enrichment":
        with get_conn() as conn:
            cur = conn.execute(
                "UPDATE channels SET tmdb_id = NULL, tmdb_type = NULL, poster_path = NULL, "
                "backdrop_path = NULL, overview = NULL, release_year = NULL, rating = NULL, "
                "genre_names = NULL, enriched_at = NULL "
                "WHERE enriched_at IS NOT NULL OR tmdb_id IS NOT NULL"
            )
            conn.commit()
            return {"cleared": cur.rowcount}

    raise HTTPException(status_code=404, detail=f"Unknown command: {cmd}")


@router.get("/local-thumbnail")
async def local_thumbnail(path: str) -> Response:
    cached = await asyncio.to_thread(local_thumbnail_file, path)
    return FileResponse(cached, media_type="image/jpeg", headers={"Cache-Control": "no-cache"})


@router.get("/local-filmstrip")
async def local_filmstrip(path: str) -> Response:
    cached = await asyncio.to_thread(local_filmstrip_file, path)
    return FileResponse(cached, media_type="image/jpeg", headers={"Cache-Control": "no-cache"})


@router.get("/local-media")
async def stream_local_media(path: str) -> Response:
    p = _authorized_local_file(path, LOCAL_MEDIA_EXTENSIONS, "Local file not found")
    media_type, _ = mimetypes.guess_type(str(p))
    return FileResponse(str(p), media_type=media_type or "application/octet-stream")

from __future__ import annotations

import asyncio
import json
import os
import secrets
import socket
import sys
import threading
from typing import Any, Dict, List, Optional

import httpx
import uvicorn
from fastapi import Depends, FastAPI, Header, HTTPException, status
from pydantic import BaseModel, Field

from .extract import extract_stream
from .harvest import harvest_streams
from .search_engine import ProviderManager, search_engine
from .probe import probe_urls

TOKEN = secrets.token_urlsafe(32)

app = FastAPI(title="Lumen Sidecar", docs_url=None, redoc_url=None)


def verify_token(authorization: Optional[str] = Header(default=None)) -> None:
    if not authorization or not authorization.startswith("Bearer "):
        raise HTTPException(status_code=status.HTTP_401_UNAUTHORIZED, detail="Missing bearer token")
    provided = authorization.removeprefix("Bearer ").strip()
    if not secrets.compare_digest(provided, TOKEN):
        raise HTTPException(status_code=status.HTTP_401_UNAUTHORIZED, detail="Invalid bearer token")


class ExtractRequest(BaseModel):
    url: str


class ExtractResponse(BaseModel):
    title: str
    url: str
    headers: Dict[str, str]


class HarvestRequest(BaseModel):
    url: str


class HarvestResponse(BaseModel):
    urls: List[str]


class ProbeRequest(BaseModel):
    urls: List[str]


class ProbeResult(BaseModel):
    url: str
    live: bool
    status: Optional[int] = None
    content_type: Optional[str] = None


class ProbeResponse(BaseModel):
    results: List[ProbeResult]


class SearchRequest(BaseModel):
    query: str
    library: List[Dict[str, Any]] = Field(default_factory=list)
    providers: List[Dict[str, Any]] = Field(default_factory=list)
    check_availability: bool = True
    preferred_language: str | None = None


class SearchConfigRequest(BaseModel):
    providers: List[Dict[str, Any]] = Field(default_factory=list)


@app.get("/health", dependencies=[Depends(verify_token)])
async def health() -> dict:
    return {"status": "ok"}


@app.post("/extract", response_model=ExtractResponse, dependencies=[Depends(verify_token)])
async def extract_endpoint(req: ExtractRequest) -> dict:
    try:
        return await extract_stream(req.url)
    except ValueError as e:
        raise HTTPException(status_code=status.HTTP_400_BAD_REQUEST, detail=str(e))
    except Exception as e:
        raise HTTPException(status_code=status.HTTP_502_BAD_GATEWAY, detail=str(e))


@app.post("/harvest", response_model=HarvestResponse, dependencies=[Depends(verify_token)])
async def harvest_endpoint(req: HarvestRequest) -> dict:
    try:
        urls = await harvest_streams(req.url)
        return {"urls": urls}
    except ValueError as e:
        raise HTTPException(status_code=status.HTTP_400_BAD_REQUEST, detail=str(e))
    except Exception as e:
        raise HTTPException(status_code=status.HTTP_502_BAD_GATEWAY, detail=str(e))


@app.post("/probe", response_model=ProbeResponse, dependencies=[Depends(verify_token)])
async def probe_endpoint(req: ProbeRequest) -> dict:
    results = await probe_urls(req.urls)
    return {"results": results}


@app.post("/search/providers/validate", dependencies=[Depends(verify_token)])
async def validate_search_providers_endpoint(req: SearchConfigRequest) -> dict:
    try:
        async with httpx.AsyncClient(follow_redirects=False) as client:
            ProviderManager(client).build([], req.providers)
        return {"valid": True}
    except ValueError as error:
        raise HTTPException(status_code=status.HTTP_400_BAD_REQUEST, detail=str(error)) from error


@app.post("/search", dependencies=[Depends(verify_token)])
async def search_endpoint(req: SearchRequest) -> dict:
    providers = req.providers
    try:
        return await search_engine.search(
            req.query,
            req.library,
            providers,
            check_availability=req.check_availability,
            preferred_language=req.preferred_language,
        )
    except ValueError as error:
        raise HTTPException(status_code=status.HTTP_400_BAD_REQUEST, detail=str(error)) from error


def _find_free_port() -> int:
    fixed = os.environ.get("LUMEN_SIDECAR_PORT")
    if fixed:
        return int(fixed)
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        return int(s.getsockname()[1])


def _watch_stdin(server: uvicorn.Server) -> None:
    try:
        for line in sys.stdin:
            if line.strip() == "SHUTDOWN":
                server.should_exit = True
                return
    except Exception:
        pass
    server.should_exit = True


async def _serve() -> None:
    port = _find_free_port()
    config = uvicorn.Config(
        app,
        host="127.0.0.1",
        port=port,
        log_level="warning",
        access_log=False,
    )
    server = uvicorn.Server(config)

    stdin_thread = threading.Thread(target=_watch_stdin, args=(server,), daemon=True)
    stdin_thread.start()

    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind(("127.0.0.1", port))
    sock.listen(128)
    sock.setblocking(False)

    ready_payload = json.dumps({"ready": True, "port": port, "token": TOKEN})
    sys.stdout.write(ready_payload + "\n")
    sys.stdout.flush()

    await server.serve(sockets=[sock])


def main() -> None:
    try:
        asyncio.run(_serve())
    except KeyboardInterrupt:
        os._exit(0)


if __name__ == "__main__":
    main()

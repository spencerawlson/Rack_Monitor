"""FastAPI application: local API and dashboard host.

Live updates are delivered with Server-Sent Events rather than WebSockets.
SSE needs no extra protocol dependency, reconnects on its own in the browser,
and one-way metric delivery is all the dashboard requires.

The server binds to 127.0.0.1 by default and no CORS origins are permitted:
the dashboard is served from the same origin as the API, so the browser needs
no cross-origin access, and nothing is exposed beyond the loopback interface.
"""

from __future__ import annotations

import asyncio
import json
from contextlib import asynccontextmanager
from pathlib import Path
from typing import AsyncIterator

from fastapi import FastAPI, Request
from fastapi.responses import FileResponse, JSONResponse, StreamingResponse
from fastapi.staticfiles import StaticFiles

from backend import config
from backend.models.metrics import DashboardSnapshot
from backend.services.metrics_service import VERSION, MetricsService

FRONTEND_DIR = Path(__file__).resolve().parent.parent / "frontend"

service = MetricsService(config.settings)


@asynccontextmanager
async def lifespan(app: FastAPI) -> AsyncIterator[None]:
    """Start the collection loops with the app and stop them with it."""
    await service.start()
    try:
        yield
    finally:
        await service.stop()


app = FastAPI(
    title="PLH Rack Monitor",
    version=VERSION,
    lifespan=lifespan,
    docs_url=None,
    redoc_url=None,
)

app.mount("/static", StaticFiles(directory=FRONTEND_DIR), name="static")


def _validated(snapshot: dict) -> dict:
    """Round-trip the snapshot through the schema before it leaves the process."""
    return DashboardSnapshot.model_validate(snapshot).model_dump(mode="json")


@app.get("/")
async def index() -> FileResponse:
    # The dashboard runs for days on a wall display; caching it would hide an
    # update after a restart.
    return FileResponse(
        FRONTEND_DIR / "index.html",
        headers={"Cache-Control": "no-store, must-revalidate"},
    )


@app.get("/api/health")
async def health() -> JSONResponse:
    """Liveness plus per-collector state. Used by the startup script."""
    snapshot = await service.get_snapshot()
    payload = {
        "status": "ok",
        "service": snapshot["service"],
        "generated_at": snapshot["generated_at"],
    }
    return JSONResponse(payload, headers={"Cache-Control": "no-store"})


@app.get("/api/config")
async def configuration() -> JSONResponse:
    """Thresholds, intervals and node labels. Contains no credentials."""
    return JSONResponse(
        config.settings.public_dict(), headers={"Cache-Control": "no-store"}
    )


@app.get("/api/metrics", response_model=DashboardSnapshot)
async def metrics() -> dict:
    """Current readings. Polling fallback for clients without SSE."""
    return await service.get_snapshot()


@app.get("/api/stream")
async def stream(request: Request) -> StreamingResponse:
    """Server-Sent Events carrying one snapshot per push interval."""

    async def event_source() -> AsyncIterator[bytes]:
        queue = await service.subscribe()
        try:
            while True:
                if await request.is_disconnected():
                    break
                try:
                    snapshot = await asyncio.wait_for(queue.get(), timeout=10.0)
                except asyncio.TimeoutError:
                    # A comment frame keeps the connection open through any
                    # idle-timeout without sending a spurious metric update.
                    yield b": keepalive\n\n"
                    continue
                payload = json.dumps(_validated(snapshot), separators=(",", ":"))
                yield f"event: metrics\ndata: {payload}\n\n".encode("utf-8")
        except asyncio.CancelledError:
            raise
        finally:
            service.unsubscribe(queue)

    return StreamingResponse(
        event_source(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-store",
            "Connection": "keep-alive",
            "X-Accel-Buffering": "no",
        },
    )


def run() -> None:
    """Entry point used by start_monitor.ps1."""
    import uvicorn

    uvicorn.run(
        app,
        host=config.settings.app_host,
        port=config.settings.app_port,
        log_level="warning",
        access_log=False,
    )


if __name__ == "__main__":
    run()

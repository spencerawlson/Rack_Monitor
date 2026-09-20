"""API surface: schema validity, placeholder nodes and credential safety."""

from __future__ import annotations

import asyncio
import json

import pytest
from fastapi.testclient import TestClient
from starlette.requests import Request

from backend.models.metrics import DashboardSnapshot
from backend.services.metrics_service import MetricsService


@pytest.fixture
def api_settings(make_settings):
    """Settings for API tests.

    The two PowerShell-backed collectors are switched off here: they would add
    several seconds of subprocess startup to every test without exercising any
    API behaviour. Their own behaviour is covered in test_temperature.py.
    """
    return make_settings(
        TEMP_ENABLED="false",
        DISK_HEALTH_ENABLED="false",
        INTERVAL_PROCESSES="600",
    )


@pytest.fixture
def fast_processes(monkeypatch):
    """Skip the full process walk, which costs seconds on a busy host."""
    from backend.collectors.windows_collector import WindowsCollector

    monkeypatch.setattr(
        WindowsCollector,
        "sample_processes",
        lambda self: {"total": 1, "running": 1, "sleeping": 0, "threads": 1},
    )


@pytest.fixture
def client(api_settings, fast_processes, monkeypatch):
    """A client whose app uses freshly loaded settings."""
    from backend import main

    monkeypatch.setattr(main.config, "settings", api_settings)
    monkeypatch.setattr(main, "service", MetricsService(api_settings))
    with TestClient(main.app) as test_client:
        yield test_client


def test_health_reports_collector_state(client):
    response = client.get("/api/health")
    assert response.status_code == 200
    payload = response.json()
    assert payload["status"] == "ok"
    collectors = payload["service"]["collectors"]
    for name in ("cpu_mem", "net", "disk", "temperature", "disk_health", "proxmox"):
        assert name in collectors


def test_config_exposes_thresholds_and_intervals(client):
    payload = client.get("/api/config").json()
    assert payload["thresholds"]["usage"]["warning"] == 70.0
    assert payload["thresholds"]["usage"]["critical"] == 90.0
    # Temperature limits are carried separately from utilisation limits.
    assert payload["thresholds"]["temperature"]["critical"] == 90.0
    assert payload["intervals_seconds"]["cpu_mem"] > 0
    assert payload["target_resolution"] == {"width": 1424, "height": 280}


def test_config_contains_no_credentials(make_settings, monkeypatch):
    secret = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
    settings = make_settings(
        PROXMOX_NODE_1_HOST="10.20.20.11",
        PROXMOX_NODE_1_TOKEN_ID="monitor@pve!plh",
        PROXMOX_NODE_1_TOKEN_SECRET=secret,
    )
    from backend import main

    monkeypatch.setattr(main.config, "settings", settings)
    monkeypatch.setattr(main, "service", MetricsService(settings))
    with TestClient(main.app) as test_client:
        body = test_client.get("/api/config").text
    assert secret not in body
    assert "monitor@pve!plh" not in body
    assert "token" not in body.lower()


def test_metrics_matches_the_published_schema(client):
    payload = client.get("/api/metrics").json()
    # Validation raises if a field has drifted from the model.
    snapshot = DashboardSnapshot.model_validate(payload)
    assert snapshot.generated_at
    assert set(snapshot.proxmox) == {"node1", "node2"}


def test_unconfigured_nodes_show_no_invented_percentages(client):
    payload = client.get("/api/metrics").json()
    for key in ("node1", "node2"):
        node = payload["proxmox"][key]
        assert node["status"] == "UNCONFIGURED"
        assert node["primary"]["cpu_percent"] is None
        assert node["primary"]["memory_percent"] is None
        assert node["primary"]["disk_percent"] is None


def test_windows_section_is_present_without_any_proxmox_node(client):
    """Windows monitoring does not depend on a node being configured."""
    windows = client.get("/api/metrics").json()["windows"]
    assert windows["memory"]["total_bytes"] > 0
    assert windows["system"]["hostname"]
    assert isinstance(windows["filesystems"], list)


def test_temperature_is_not_fabricated(client):
    temperature = client.get("/api/metrics").json()["windows"]["temperature"]
    if not temperature["available"]:
        assert temperature["cpu_celsius"] is None
        assert temperature["detail"]


def test_dashboard_is_served_without_caching(client):
    response = client.get("/")
    assert response.status_code == 200
    assert "no-store" in response.headers.get("cache-control", "")
    assert "PLH RACK MONITOR" in response.text


def test_static_assets_are_served(client):
    for path in ("/static/app.js", "/static/styles.css"):
        assert client.get(path).status_code == 200


@pytest.mark.asyncio
async def test_stream_emits_a_metrics_event(api_settings, fast_processes, monkeypatch):
    """The SSE endpoint delivers a complete, schema-valid snapshot.

    The endpoint is driven directly rather than through TestClient: the stream
    is deliberately endless, and a test client waits for the generator to
    finish when it closes the response.
    """
    from backend import main

    monkeypatch.setattr(main.config, "settings", api_settings)
    service = MetricsService(api_settings)
    monkeypatch.setattr(main, "service", service)
    await service.start()

    scope = {
        "type": "http",
        "method": "GET",
        "path": "/api/stream",
        "headers": [],
        "query_string": b"",
        "client": ("plh.test", 1),
    }

    async def receive():
        # A client that stays connected: the endpoint checks for a disconnect
        # message and must not see one while the frame is being read.
        await asyncio.Event().wait()

    data_line = None
    try:
        response = await main.stream(Request(scope, receive))
        assert response.media_type == "text/event-stream"
        frames = response.body_iterator
        try:
            raw = await asyncio.wait_for(frames.__anext__(), timeout=10.0)
            for line in raw.decode("utf-8").splitlines():
                if line.startswith("data:"):
                    data_line = line[5:].strip()
        finally:
            await frames.aclose()
    finally:
        await service.stop()

    assert data_line
    # Each frame is a complete snapshot, so a client that misses one recovers
    # on the next.
    DashboardSnapshot.model_validate(json.loads(data_line))

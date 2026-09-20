"""Proxmox collection against recorded responses. No node is required."""

from __future__ import annotations

import httpx
import pytest

from backend.collectors.proxmox_collector import ProxmoxCollector, percent_of
from backend.config import (
    STATUS_AUTH_ERROR,
    STATUS_CONFIG_ERROR,
    STATUS_OFFLINE,
    STATUS_ONLINE,
    STATUS_UNCONFIGURED,
)

SECRET = "11111111-2222-3333-4444-555555555555"

# A representative /nodes/{node}/status payload from Proxmox VE 8.
STATUS_PAYLOAD = {
    "data": {
        "uptime": 1036800,
        "cpu": 0.1834,
        "loadavg": ["0.42", "0.35", "0.30"],
        "cpuinfo": {"cpus": 8, "model": "Intel(R) Core(TM) i5-6500T", "mhz": "2500.000"},
        "memory": {"total": 33645936640, "used": 12163481600, "free": 21482455040},
        "rootfs": {"total": 100861792256, "used": 22194724864, "avail": 73513623552},
        "swap": {"total": 8589934592, "used": 268435456, "free": 8321499136},
        "pveversion": "pve-manager/8.2.2/9355359cd7afbae4",
        "kversion": "Linux 6.8.4-2-pve",
    }
}


def _configured(make_settings, **overrides):
    env = {
        "PROXMOX_NODE_1_HOST": "10.20.20.11",
        "PROXMOX_NODE_1_TOKEN_ID": "monitor@pve!plh",
        "PROXMOX_NODE_1_TOKEN_SECRET": SECRET,
        "PROXMOX_NODE_1_API_NODE": "pve01",
        "PROXMOX_NODE_1_TIMEOUT_SECONDS": "1",
    }
    env.update(overrides)
    settings = make_settings(**env)
    return settings, settings.node("node1")


def _handler(routes):
    """Build a transport that answers from a path -> (status, json) mapping."""

    def handle(request: httpx.Request) -> httpx.Response:
        for suffix, (code, payload) in routes.items():
            if request.url.path.endswith(suffix):
                return httpx.Response(code, json=payload)
        return httpx.Response(404, json={"data": None})

    return httpx.MockTransport(handle)


# ------------------------------------------------------------- percentages


def test_percent_of_handles_strings_and_zero():
    assert percent_of("50", "200") == 25.0
    assert percent_of(1, 0) is None
    assert percent_of(None, 10) is None


def test_node_percentages_are_derived_correctly(make_settings):
    settings, node = _configured(make_settings)
    collector = ProxmoxCollector(node, settings)
    result = collector._build(STATUS_PAYLOAD["data"], "pve01")

    assert result["cpu_percent"] == 18.3
    assert result["memory_percent"] == percent_of(12163481600, 33645936640) == 36.2
    assert result["rootfs_percent"] == percent_of(22194724864, 100861792256) == 22.0
    assert result["swap_percent"] == 3.1
    assert result["cpu_count"] == 8
    assert result["uptime_seconds"] == 1036800
    assert result["load"]["min1"] == 0.42
    assert result["pve_version"].startswith("pve-manager/8.2.2")
    # The donut inputs mirror the detailed values.
    assert result["primary"]["cpu_percent"] == 18.3
    assert result["primary"]["disk_percent"] == 22.0


def test_node_temperature_is_never_invented(make_settings):
    settings, node = _configured(make_settings)
    result = ProxmoxCollector(node, settings)._build(STATUS_PAYLOAD["data"], "pve01")
    assert result["temperature"]["available"] is False
    assert result["temperature"]["cpu_celsius"] is None


# --------------------------------------------------------- unconfigured


@pytest.mark.asyncio
async def test_unconfigured_node_shows_a_placeholder_with_no_figures(settings):
    node = settings.node("node1")
    result = await ProxmoxCollector(node, settings).collect()
    assert result["status"] == STATUS_UNCONFIGURED
    assert result["message"] == "Host not set"
    assert result["primary"]["cpu_percent"] is None
    assert result["primary"]["memory_percent"] is None
    assert result["primary"]["disk_percent"] is None


@pytest.mark.asyncio
async def test_config_error_is_distinct_from_unconfigured(make_settings):
    settings, node = _configured(make_settings, PROXMOX_NODE_1_TOKEN_ID="broken")
    result = await ProxmoxCollector(node, settings).collect()
    assert result["status"] == STATUS_CONFIG_ERROR


# ---------------------------------------------------------------- success


@pytest.mark.asyncio
async def test_successful_collection(make_settings):
    settings, node = _configured(make_settings)
    routes = {
        "/nodes": (200, {"data": [{"node": "pve01"}]}),
        "/nodes/pve01/status": (200, STATUS_PAYLOAD),
        "/nodes/pve01/storage": (
            200,
            {
                "data": [
                    {"storage": "local", "type": "dir", "total": 100, "used": 40, "avail": 60},
                    {"storage": "local-lvm", "type": "lvmthin", "total": 200, "used": 50, "avail": 150},
                ]
            },
        ),
        "/nodes/pve01/qemu": (
            200,
            {"data": [{"vmid": 100, "status": "running"}, {"vmid": 101, "status": "stopped"}]},
        ),
        "/nodes/pve01/lxc": (200, {"data": [{"vmid": 200, "status": "running"}]}),
    }
    collector = ProxmoxCollector(node, settings, transport=_handler(routes))
    result = await collector.collect()
    await collector.aclose()

    assert result["status"] == STATUS_ONLINE
    assert result["stale"] is False
    assert result["last_success"] is not None
    assert result["vms"] == {"total": 2, "running": 1, "permitted": True}
    assert result["containers"] == {"total": 1, "running": 1, "permitted": True}
    assert result["storage_total_bytes"] == 300
    assert result["storage_available_bytes"] == 210
    assert len(result["storage"]) == 2


@pytest.mark.asyncio
async def test_authorization_header_carries_the_token(make_settings):
    settings, node = _configured(make_settings)
    seen: dict[str, str] = {}

    def handle(request: httpx.Request) -> httpx.Response:
        seen["auth"] = request.headers.get("authorization", "")
        if request.url.path.endswith("/status"):
            return httpx.Response(200, json=STATUS_PAYLOAD)
        return httpx.Response(200, json={"data": []})

    collector = ProxmoxCollector(node, settings, transport=httpx.MockTransport(handle))
    await collector.collect()
    await collector.aclose()
    assert seen["auth"] == f"PVEAPIToken=monitor@pve!plh={SECRET}"


# ------------------------------------------------------------ auth failure


@pytest.mark.asyncio
async def test_authentication_failure_is_reported_as_auth_error(make_settings):
    settings, node = _configured(make_settings)
    routes = {"/status": (401, {"data": None}), "/nodes": (401, {"data": None})}
    collector = ProxmoxCollector(node, settings, transport=_handler(routes))
    result = await collector.collect()
    await collector.aclose()

    assert result["status"] == STATUS_AUTH_ERROR
    assert "401" in result["message"]
    assert SECRET not in result["message"]


@pytest.mark.asyncio
async def test_forbidden_status_endpoint_is_an_auth_error(make_settings):
    settings, node = _configured(make_settings)
    routes = {"/nodes/pve01/status": (403, {"data": None}), "/nodes": (200, {"data": [{"node": "pve01"}]})}
    collector = ProxmoxCollector(node, settings, transport=_handler(routes))
    result = await collector.collect()
    await collector.aclose()
    assert result["status"] == STATUS_AUTH_ERROR
    assert "PVEAuditor" in result["message"]


@pytest.mark.asyncio
async def test_forbidden_subresource_is_not_fatal(make_settings):
    """A token allowed to read node status but not guests still yields metrics."""
    settings, node = _configured(make_settings)
    routes = {
        "/nodes/pve01/status": (200, STATUS_PAYLOAD),
        "/nodes/pve01/storage": (403, {"data": None}),
        "/nodes/pve01/qemu": (403, {"data": None}),
        "/nodes/pve01/lxc": (403, {"data": None}),
        "/nodes": (200, {"data": [{"node": "pve01"}]}),
    }
    collector = ProxmoxCollector(node, settings, transport=_handler(routes))
    result = await collector.collect()
    await collector.aclose()

    assert result["status"] == STATUS_ONLINE
    assert result["cpu_percent"] == 18.3
    assert result["vms"]["permitted"] is False
    assert result["vms"]["total"] is None
    assert result["storage"] == []


# ----------------------------------------------------------- disconnection


@pytest.mark.asyncio
async def test_disconnection_keeps_the_last_good_reading_and_marks_it_stale(make_settings):
    settings, node = _configured(make_settings)

    state = {"up": True}

    def handle(request: httpx.Request) -> httpx.Response:
        if not state["up"]:
            raise httpx.ConnectError("node is switched off", request=request)
        if request.url.path.endswith("/status"):
            return httpx.Response(200, json=STATUS_PAYLOAD)
        return httpx.Response(200, json={"data": []})

    collector = ProxmoxCollector(node, settings, transport=httpx.MockTransport(handle))
    good = await collector.collect()
    assert good["status"] == STATUS_ONLINE

    state["up"] = False
    offline = await collector.collect(previous=good)
    await collector.aclose()

    assert offline["status"] == STATUS_OFFLINE
    assert offline["stale"] is True
    # The figures and the timestamp of the last success are preserved so the
    # dashboard can show how old they are.
    assert offline["cpu_percent"] == good["cpu_percent"]
    assert offline["memory_percent"] == good["memory_percent"]
    assert offline["last_success"] == good["last_success"]
    assert offline["last_attempt"] is not None


@pytest.mark.asyncio
async def test_a_node_that_was_never_reachable_shows_no_figures(make_settings):
    settings, node = _configured(make_settings)

    def handle(request: httpx.Request) -> httpx.Response:
        raise httpx.ConnectError("unreachable", request=request)

    collector = ProxmoxCollector(node, settings, transport=httpx.MockTransport(handle))
    result = await collector.collect(previous=None)
    await collector.aclose()

    assert result["status"] == STATUS_OFFLINE
    assert result["stale"] is False
    assert result["primary"]["cpu_percent"] is None


@pytest.mark.asyncio
async def test_timeout_is_reported_without_raising(make_settings):
    settings, node = _configured(make_settings)

    def handle(request: httpx.Request) -> httpx.Response:
        raise httpx.ReadTimeout("too slow", request=request)

    collector = ProxmoxCollector(node, settings, transport=httpx.MockTransport(handle))
    result = await collector.collect()
    await collector.aclose()
    assert result["status"] == STATUS_OFFLINE
    assert "Timed out" in result["message"]


@pytest.mark.asyncio
async def test_repeated_failures_back_off(make_settings):
    settings, node = _configured(make_settings)
    calls = {"count": 0}

    def handle(request: httpx.Request) -> httpx.Response:
        calls["count"] += 1
        raise httpx.ConnectError("down", request=request)

    collector = ProxmoxCollector(node, settings, transport=httpx.MockTransport(handle))
    await collector.collect()
    attempts_after_first = calls["count"]
    result = await collector.collect()
    await collector.aclose()

    # The second call is inside the backoff window and makes no request.
    assert calls["count"] == attempts_after_first
    assert "next retry in" in result["message"]


# --------------------------------------------------- response validation


@pytest.mark.asyncio
async def test_missing_rootfs_does_not_make_a_reachable_node_offline(make_settings):
    """Absent optional fields yield None, not a false OFFLINE verdict."""
    settings, node = _configured(make_settings)
    trimmed = {"data": {"cpu": 0.5, "memory": {"total": 100, "used": 50}, "uptime": 60}}
    routes = {
        "/nodes/pve01/status": (200, trimmed),
        "/nodes": (200, {"data": [{"node": "pve01"}]}),
    }
    collector = ProxmoxCollector(node, settings, transport=_handler(routes))
    result = await collector.collect()
    await collector.aclose()

    assert result["status"] == STATUS_ONLINE
    assert result["cpu_percent"] == 50.0
    assert result["memory_percent"] == 50.0
    assert result["rootfs_percent"] is None
    assert result["primary"]["disk_percent"] is None


@pytest.mark.asyncio
async def test_payload_without_a_data_member_is_rejected(make_settings):
    settings, node = _configured(make_settings)
    routes = {
        "/nodes/pve01/status": (200, {"unexpected": True}),
        "/nodes": (200, {"data": [{"node": "pve01"}]}),
    }
    collector = ProxmoxCollector(node, settings, transport=_handler(routes))
    result = await collector.collect()
    await collector.aclose()
    assert result["status"] == STATUS_OFFLINE
    assert "Invalid API response" in result["message"]


@pytest.mark.asyncio
async def test_non_numeric_values_are_ignored_rather_than_crashing(make_settings):
    settings, node = _configured(make_settings)
    garbage = {
        "data": {
            "cpu": "not-a-number",
            "memory": {"total": "abc", "used": None},
            "rootfs": [],
            "uptime": None,
            "loadavg": "not-a-list",
        }
    }
    routes = {
        "/nodes/pve01/status": (200, garbage),
        "/nodes": (200, {"data": [{"node": "pve01"}]}),
    }
    collector = ProxmoxCollector(node, settings, transport=_handler(routes))
    result = await collector.collect()
    await collector.aclose()

    assert result["status"] == STATUS_ONLINE
    assert result["cpu_percent"] is None
    assert result["memory_percent"] is None
    assert result["rootfs_percent"] is None
    assert result["load"]["min1"] is None


@pytest.mark.asyncio
async def test_node_name_is_discovered_when_the_configured_one_differs(make_settings):
    settings, node = _configured(make_settings, PROXMOX_NODE_1_API_NODE="wrong-name")
    routes = {
        "/nodes/pve-real/status": (200, STATUS_PAYLOAD),
        "/nodes": (200, {"data": [{"node": "pve-real"}]}),
    }
    collector = ProxmoxCollector(node, settings, transport=_handler(routes))
    result = await collector.collect()
    await collector.aclose()

    assert result["status"] == STATUS_ONLINE
    assert result["api_node"] == "pve-real"


@pytest.mark.asyncio
async def test_error_messages_never_contain_the_token(make_settings):
    settings, node = _configured(make_settings)

    def handle(request: httpx.Request) -> httpx.Response:
        raise httpx.ConnectError(f"failed talking to {SECRET}", request=request)

    collector = ProxmoxCollector(node, settings, transport=httpx.MockTransport(handle))
    result = await collector.collect()
    await collector.aclose()
    assert SECRET not in result["message"]
    assert "[redacted]" in result["message"]


def test_only_per_node_endpoints_are_used(make_settings):
    """Cluster-wide endpoints would double-count two independent nodes."""
    source = (__import__("pathlib").Path(__file__).parent.parent
              / "backend" / "collectors" / "proxmox_collector.py").read_text(encoding="utf-8")
    assert "/cluster/resources" not in source

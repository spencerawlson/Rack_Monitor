"""Temperature and drive health: absent sensors report N/A, never a number."""

from __future__ import annotations

import subprocess

import pytest

from backend.collectors.health_collector import HealthCollector
from backend.collectors.temperature_collector import TemperatureCollector


def _no_sources(collector: TemperatureCollector, monkeypatch) -> None:
    """Make every temperature source unavailable."""
    monkeypatch.setattr(collector, "_from_psutil", lambda: (None, None))

    async def no_http():
        return (None, None)

    async def no_wmi():
        return (None, None)

    monkeypatch.setattr(collector, "_from_lhm_http", no_http)
    monkeypatch.setattr(collector, "_from_wmi", no_wmi)


@pytest.mark.asyncio
async def test_missing_sensors_report_unavailable(settings, monkeypatch):
    collector = TemperatureCollector(settings)
    _no_sources(collector, monkeypatch)

    reading = await collector.read()
    assert reading["cpu_celsius"] is None
    assert reading["available"] is False
    assert "No supported sensor source" in reading["detail"]


@pytest.mark.asyncio
async def test_failed_probe_backs_off_instead_of_retrying_every_interval(settings, monkeypatch):
    collector = TemperatureCollector(settings)
    calls = {"count": 0}

    def counted():
        calls["count"] += 1
        return (None, None)

    monkeypatch.setattr(collector, "_from_psutil", counted)

    async def no_http():
        return (None, None)

    async def no_wmi():
        return (None, None)

    monkeypatch.setattr(collector, "_from_lhm_http", no_http)
    monkeypatch.setattr(collector, "_from_wmi", no_wmi)

    await collector.read()
    second = await collector.read()

    # The second call is inside the backoff window and probes nothing.
    assert calls["count"] == 1
    assert second["available"] is False
    assert "retrying in" in second["detail"]


@pytest.mark.asyncio
async def test_disabling_temperature_skips_probing(make_settings):
    settings = make_settings(TEMP_ENABLED="false")
    reading = await TemperatureCollector(settings).read()
    assert reading["available"] is False
    assert "Disabled by configuration" in reading["detail"]


@pytest.mark.asyncio
async def test_libre_hardware_monitor_http_payload_is_parsed(settings, monkeypatch):
    collector = TemperatureCollector(settings)
    monkeypatch.setattr(collector, "_from_psutil", lambda: (None, None))

    payload = {
        "Text": "Sensor",
        "Children": [
            {
                "Text": "MiniPC",
                "Children": [
                    {
                        "Text": "Intel Core i5",
                        "Children": [
                            {
                                "Text": "Temperatures",
                                "Children": [
                                    {"Text": "CPU Core #1", "Value": "44.0 °C"},
                                    {"Text": "CPU Package", "Value": "47.5 °C"},
                                    {"Text": "Core Max", "Value": "48.0 °C"},
                                ],
                            }
                        ],
                    }
                ],
            }
        ],
    }

    readings: list[float] = []
    collector._walk_lhm(payload, readings)
    # CPU Core #1 and CPU Package are recognised; "Core Max" is not a CPU
    # package label, so it is left out rather than guessed at.
    assert readings == [44.0, 47.5]
    assert max(readings) == 47.5


def test_lhm_parser_ignores_non_temperature_values(settings):
    collector = TemperatureCollector(settings)
    readings: list[float] = []
    collector._walk_lhm(
        {"Text": "CPU Package", "Value": "3400 MHz", "Children": []}, readings
    )
    assert readings == []


@pytest.mark.asyncio
async def test_disk_health_reports_unavailable_when_powershell_is_missing(settings, monkeypatch):
    def missing(*args, **kwargs):
        raise FileNotFoundError("powershell.exe")

    monkeypatch.setattr(subprocess, "run", missing)
    result = await HealthCollector(settings).disk_health()
    assert result["available"] is False
    assert result["disks"] == []
    assert "PowerShell" in result["detail"]


@pytest.mark.asyncio
async def test_disk_health_handles_a_timeout(settings, monkeypatch):
    def slow(*args, **kwargs):
        raise subprocess.TimeoutExpired(cmd="powershell", timeout=20)

    monkeypatch.setattr(subprocess, "run", slow)
    result = await HealthCollector(settings).disk_health()
    assert result["available"] is False
    assert "timed out" in result["detail"]


@pytest.mark.asyncio
async def test_disk_health_parses_a_single_disk_object(settings, monkeypatch):
    class Completed:
        returncode = 0
        stdout = (
            '{"FriendlyName":"G521N 256G","MediaType":4,'
            '"HealthStatus":"Healthy","OperationalStatus":"OK","Size":256060514304}'
        )

    monkeypatch.setattr(subprocess, "run", lambda *a, **k: Completed())
    result = await HealthCollector(settings).disk_health()
    assert result["available"] is True
    assert len(result["disks"]) == 1
    disk = result["disks"][0]
    assert disk["name"] == "G521N 256G"
    # A numeric MediaType is mapped to a readable name.
    assert disk["media_type"] == "SSD"
    assert disk["health"] == "Healthy"


@pytest.mark.asyncio
async def test_disk_health_handles_unreadable_output(settings, monkeypatch):
    class Completed:
        returncode = 0
        stdout = "not json at all"

    monkeypatch.setattr(subprocess, "run", lambda *a, **k: Completed())
    result = await HealthCollector(settings).disk_health()
    assert result["available"] is False
    assert "unreadable" in result["detail"]


@pytest.mark.asyncio
async def test_disk_health_flattens_list_valued_status(settings, monkeypatch):
    class Completed:
        returncode = 0
        stdout = (
            '[{"FriendlyName":"NXT 256GB","MediaType":"SSD","HealthStatus":"Warning",'
            '"OperationalStatus":["OK","Degraded"],"Size":256060514304}]'
        )

    monkeypatch.setattr(subprocess, "run", lambda *a, **k: Completed())
    result = await HealthCollector(settings).disk_health()
    assert result["disks"][0]["operational"] == "OK, Degraded"

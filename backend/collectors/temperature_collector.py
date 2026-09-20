"""CPU temperature collection.

Windows exposes no temperature to an unprivileged process through psutil, so
three optional sources are probed in order of cost:

1. psutil.sensors_temperatures - present on Linux, absent on Windows.
2. LibreHardwareMonitor's built-in web server (JSON over HTTP on localhost).
3. LibreHardwareMonitor or OpenHardwareMonitor WMI namespace via PowerShell.

When every source fails the reading is None and the dashboard shows N/A. A
value is never estimated, interpolated or carried over from a stale probe.
Failed probing backs off so that an unsupported host is not charged a
PowerShell process launch on every interval.
"""

from __future__ import annotations

import asyncio
import re
import subprocess
import time
from typing import Any

import httpx
import psutil

from backend.config import Settings

# Sensor labels that plausibly describe a whole-package CPU temperature.
_CPU_LABEL = re.compile(r"cpu|package|tdie|tctl|core average", re.IGNORECASE)

_WMI_SCRIPT = (
    "$ErrorActionPreference='Stop';"
    "$out=@();"
    "foreach($ns in 'root/LibreHardwareMonitor','root/OpenHardwareMonitor'){"
    "  try{"
    "    $s=Get-CimInstance -Namespace $ns -ClassName Sensor -ErrorAction Stop |"
    "       Where-Object { $_.SensorType -eq 'Temperature' };"
    "    foreach($i in $s){ $out += ('{0}|{1}' -f $i.Name, $i.Value) }"
    "  }catch{}"
    "}"
    "$out -join \"`n\""
)


class TemperatureCollector:
    """Probes the available temperature sources, with backoff on failure."""

    def __init__(self, settings: Settings) -> None:
        self.settings = settings
        self._next_probe_allowed = 0.0
        self._source: str | None = None
        self._failures = 0

    def _unavailable(self, detail: str) -> dict[str, Any]:
        return {"cpu_celsius": None, "source": None, "available": False, "detail": detail}

    async def read(self) -> dict[str, Any]:
        """Return the current CPU temperature, or an unavailable result."""
        if not self.settings.temp_enabled:
            return self._unavailable("Disabled by configuration (TEMP_ENABLED=false)")

        now = time.monotonic()
        if now < self._next_probe_allowed:
            remaining = int(self._next_probe_allowed - now)
            return self._unavailable(f"No sensor source; retrying in {remaining}s")

        value, source = self._from_psutil()
        if value is None:
            value, source = await self._from_lhm_http()
        if value is None:
            value, source = await self._from_wmi()

        if value is None:
            self._failures += 1
            # Repeated failure means the host has no supported source; probing
            # slows to the configured backoff instead of every interval.
            self._next_probe_allowed = now + self.settings.sensor_backoff_seconds
            return self._unavailable(
                "No supported sensor source (psutil, LibreHardwareMonitor HTTP, WMI all unavailable)"
            )

        self._failures = 0
        self._source = source
        return {
            "cpu_celsius": round(float(value), 1),
            "source": source,
            "available": True,
            "detail": None,
        }

    # ------------------------------------------------------------------------

    def _from_psutil(self) -> tuple[float | None, str | None]:
        reader = getattr(psutil, "sensors_temperatures", None)
        if reader is None:
            return None, None
        try:
            groups = reader()
        except Exception:
            return None, None
        if not groups:
            return None, None

        labelled: list[float] = []
        any_value: float | None = None
        for entries in groups.values():
            for entry in entries:
                current = getattr(entry, "current", None)
                if current is None:
                    continue
                if any_value is None:
                    any_value = float(current)
                if _CPU_LABEL.search(getattr(entry, "label", "") or ""):
                    labelled.append(float(current))
        if labelled:
            return max(labelled), "psutil"
        if any_value is not None:
            return any_value, "psutil"
        return None, None

    async def _from_lhm_http(self) -> tuple[float | None, str | None]:
        """Read LibreHardwareMonitor's local web server, when it is running."""
        url = self.settings.lhm_http_url
        if not url:
            return None, None
        try:
            async with httpx.AsyncClient(timeout=self.settings.temp_probe_timeout) as client:
                response = await client.get(url)
                if response.status_code != 200:
                    return None, None
                payload = response.json()
        except Exception:
            return None, None

        readings: list[float] = []
        self._walk_lhm(payload, readings)
        if readings:
            return max(readings), "LibreHardwareMonitor (HTTP)"
        return None, None

    def _walk_lhm(self, node: Any, out: list[float], parent: str = "") -> None:
        """Depth-first walk of the LibreHardwareMonitor JSON tree.

        Nodes carry a Text label and a Value such as '46.0 <degree>C'; only
        Celsius temperature nodes whose label looks CPU-related are kept.
        """
        if isinstance(node, dict):
            text = str(node.get("Text", "") or "")
            value = node.get("Value")
            if isinstance(value, str) and "C" in value:
                context = f"{parent} {text}"
                if _CPU_LABEL.search(context):
                    match = re.search(r"-?\d+(?:[.,]\d+)?", value)
                    if match:
                        try:
                            out.append(float(match.group(0).replace(",", ".")))
                        except ValueError:
                            pass
            for child in node.get("Children", []) or []:
                self._walk_lhm(child, out, text or parent)
        elif isinstance(node, list):
            for child in node:
                self._walk_lhm(child, out, parent)

    async def _from_wmi(self) -> tuple[float | None, str | None]:
        """Read the LibreHardwareMonitor or OpenHardwareMonitor WMI namespace."""
        if not self.settings.lhm_wmi_enabled:
            return None, None
        try:
            completed = await asyncio.to_thread(
                subprocess.run,
                [
                    "powershell.exe",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    _WMI_SCRIPT,
                ],
                capture_output=True,
                text=True,
                timeout=max(3.0, self.settings.temp_probe_timeout),
                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
            )
        except Exception:
            return None, None
        if completed.returncode != 0 or not completed.stdout:
            return None, None

        readings: list[float] = []
        for line in completed.stdout.splitlines():
            if "|" not in line:
                continue
            name, _, raw = line.partition("|")
            if not _CPU_LABEL.search(name):
                continue
            try:
                readings.append(float(raw.strip()))
            except ValueError:
                continue
        if readings:
            return max(readings), "LibreHardwareMonitor (WMI)"
        return None, None

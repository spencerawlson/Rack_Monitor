"""Physical disk health for the Windows host.

Health comes from the Windows Storage provider (Get-PhysicalDisk), which
reports the drive's own health and operational state. It is not a full SMART
attribute dump: when the provider cannot answer, the result is unavailable and
the dashboard shows N/A rather than an assumed 'healthy'.

The query costs a PowerShell process launch, so it belongs on a slow loop.
"""

from __future__ import annotations

import asyncio
import json
import subprocess
from typing import Any

from backend.config import Settings

_DISK_HEALTH_SCRIPT = (
    "$ErrorActionPreference='Stop';"
    "Get-PhysicalDisk |"
    " Select-Object FriendlyName,MediaType,HealthStatus,OperationalStatus,Size |"
    " ConvertTo-Json -Compress -Depth 3"
)

# Older Storage providers report MediaType as an enumeration value.
_MEDIA_TYPES = {0: "Unspecified", 3: "HDD", 4: "SSD", 5: "SCM"}


def _as_text(value: Any) -> str | None:
    """Flatten a scalar or list-valued status field into readable text."""
    if value is None:
        return None
    if isinstance(value, list):
        parts = [str(item) for item in value if item is not None]
        return ", ".join(parts) if parts else None
    return str(value)


def _media_type(value: Any) -> str | None:
    if isinstance(value, int):
        return _MEDIA_TYPES.get(value, str(value))
    return _as_text(value)


class HealthCollector:
    """Queries drive health. Failure is reported, never guessed."""

    def __init__(self, settings: Settings) -> None:
        self.settings = settings

    def _unavailable(self, detail: str) -> dict[str, Any]:
        return {"available": False, "detail": detail, "disks": []}

    async def disk_health(self) -> dict[str, Any]:
        if not self.settings.disk_health_enabled:
            return self._unavailable("Disabled by configuration (DISK_HEALTH_ENABLED=false)")

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
                    _DISK_HEALTH_SCRIPT,
                ],
                capture_output=True,
                text=True,
                timeout=20.0,
                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
            )
        except FileNotFoundError:
            return self._unavailable("PowerShell not available")
        except subprocess.TimeoutExpired:
            return self._unavailable("Storage provider query timed out")
        except Exception as exc:
            return self._unavailable(f"Query failed: {type(exc).__name__}")

        if completed.returncode != 0 or not (completed.stdout or "").strip():
            return self._unavailable("Storage provider returned no data")

        try:
            payload = json.loads(completed.stdout)
        except json.JSONDecodeError:
            return self._unavailable("Storage provider returned unreadable data")

        # A single disk serialises as an object rather than a list.
        if isinstance(payload, dict):
            payload = [payload]
        if not isinstance(payload, list):
            return self._unavailable("Storage provider returned an unexpected shape")

        disks: list[dict[str, Any]] = []
        for item in payload:
            if not isinstance(item, dict):
                continue
            size = item.get("Size")
            disks.append(
                {
                    "name": _as_text(item.get("FriendlyName")),
                    "media_type": _media_type(item.get("MediaType")),
                    "health": _as_text(item.get("HealthStatus")),
                    "operational": _as_text(item.get("OperationalStatus")),
                    "size_bytes": int(size) if isinstance(size, (int, float)) else None,
                }
            )

        if not disks:
            return self._unavailable("No physical disks reported")
        return {"available": True, "detail": None, "disks": disks}

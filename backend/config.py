"""Environment-driven configuration with validation.

Configuration is read from the process environment, optionally seeded from a
.env file in the project root. Validation never raises: every problem is
recorded in Settings.errors and a safe default is substituted, so that a
malformed Proxmox entry can never prevent Windows monitoring from starting.
Secrets are held in memory only and are never included in any API payload.
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from pathlib import Path

from dotenv import load_dotenv

PROJECT_ROOT = Path(__file__).resolve().parent.parent

# The .env file is optional. Values already present in the environment win, so
# a shell override does not require the file to be edited.
load_dotenv(PROJECT_ROOT / ".env", override=False)

# Node status values shared by the backend and the frontend.
STATUS_ONLINE = "ONLINE"
STATUS_OFFLINE = "OFFLINE"
STATUS_AUTH_ERROR = "AUTH_ERROR"
STATUS_UNCONFIGURED = "UNCONFIGURED"
STATUS_CONFIG_ERROR = "CONFIG_ERROR"


def _raw(name: str, default: str = "") -> str:
    value = os.environ.get(name)
    return default if value is None else value.strip()


def _int(name: str, default: int, errors: list[str], *, low: int, high: int) -> int:
    """Return a bounded integer, recording a message when the value is unusable."""
    raw = _raw(name)
    if raw == "":
        return default
    try:
        value = int(raw)
    except ValueError:
        errors.append(f"{name}={raw!r} is not an integer; using {default}")
        return default
    if not low <= value <= high:
        errors.append(f"{name}={value} outside {low}..{high}; using {default}")
        return default
    return value


def _float(name: str, default: float, errors: list[str], *, low: float, high: float) -> float:
    """Return a bounded float, recording a message when the value is unusable."""
    raw = _raw(name)
    if raw == "":
        return default
    try:
        value = float(raw)
    except ValueError:
        errors.append(f"{name}={raw!r} is not a number; using {default}")
        return default
    if not low <= value <= high:
        errors.append(f"{name}={value} outside {low}..{high}; using {default}")
        return default
    return value


def _bool(name: str, default: bool, errors: list[str]) -> bool:
    raw = _raw(name).lower()
    if raw == "":
        return default
    if raw in {"1", "true", "yes", "on"}:
        return True
    if raw in {"0", "false", "no", "off"}:
        return False
    errors.append(f"{name}={raw!r} is not a boolean; using {default}")
    return default


@dataclass(frozen=True)
class Thresholds:
    """Display thresholds. These drive colour only; they are not a health verdict."""

    warning: float
    critical: float

    def state(self, value: float | None) -> str:
        if value is None:
            return "unavailable"
        if value >= self.critical:
            return "critical"
        if value >= self.warning:
            return "warning"
        return "normal"


@dataclass(frozen=True)
class ProxmoxNodeConfig:
    """One independently configured Proxmox VE node."""

    key: str
    index: int
    name: str
    host: str
    port: int
    api_node: str
    token_id: str = field(repr=False, default="")
    token_secret: str = field(repr=False, default="")
    verify_tls: bool = True
    ca_cert_path: str = ""
    timeout_seconds: float = 4.0
    errors: tuple[str, ...] = ()

    @property
    def configured(self) -> bool:
        """True only when enough detail exists to attempt an authenticated call."""
        return bool(self.host and self.token_id and self.token_secret and not self.errors)

    @property
    def base_url(self) -> str:
        return f"https://{self.host}:{self.port}/api2/json"

    @property
    def status_when_unreachable(self) -> str:
        if self.errors:
            return STATUS_CONFIG_ERROR
        if not self.configured:
            return STATUS_UNCONFIGURED
        return STATUS_OFFLINE

    @property
    def unconfigured_reason(self) -> str | None:
        """Reason the node is not being polled, or None when it is."""
        if self.errors:
            return "; ".join(self.errors)
        if not self.host:
            return "Host not set"
        if not self.token_id or not self.token_secret:
            return "API token not set"
        return None

    def verify_option(self) -> bool | str:
        """Value for the httpx verify argument: a CA bundle path when given, else a bool."""
        if not self.verify_tls:
            return False
        if self.ca_cert_path:
            return self.ca_cert_path
        return True

    def auth_header(self) -> dict[str, str]:
        return {"Authorization": f"PVEAPIToken={self.token_id}={self.token_secret}"}

    def public_dict(self) -> dict[str, object]:
        """Frontend-safe description. Token fields are deliberately absent."""
        return {
            "key": self.key,
            "name": self.name,
            "host": self.host or None,
            "port": self.port if self.host else None,
            "api_node": self.api_node if self.host else None,
            "configured": self.configured,
            "verify_tls": self.verify_tls,
            "reason": self.unconfigured_reason,
        }


def _load_node(index: int, errors: list[str]) -> ProxmoxNodeConfig:
    prefix = f"PROXMOX_NODE_{index}_"
    node_errors: list[str] = []

    name = _raw(prefix + "NAME") or f"PVE0{index}"
    host = _raw(prefix + "HOST")
    port = _int(prefix + "PORT", 8006, node_errors, low=1, high=65535)
    api_node = _raw(prefix + "API_NODE")
    token_id = _raw(prefix + "TOKEN_ID")
    token_secret = _raw(prefix + "TOKEN_SECRET")
    verify_tls = _bool(prefix + "VERIFY_TLS", True, node_errors)
    ca_cert_path = _raw(prefix + "CA_CERT_PATH") or _raw("PROXMOX_CA_CERT_PATH")
    timeout_seconds = _float(prefix + "TIMEOUT_SECONDS", 4.0, node_errors, low=0.5, high=30.0)

    # A token id is expected in user@realm!tokenname form. A wrong shape is a
    # configuration error, not an authentication failure to be discovered later.
    if token_id and ("!" not in token_id or "@" not in token_id):
        node_errors.append(prefix + "TOKEN_ID must look like user@realm!tokenname")

    # An unreadable CA bundle is reported rather than silently falling back to
    # the public trust store, which would change what is actually trusted.
    if ca_cert_path and not Path(ca_cert_path).exists():
        node_errors.append(f"CA certificate not found: {ca_cert_path}")
        ca_cert_path = ""

    if host and not verify_tls:
        errors.append(
            prefix + f"VERIFY_TLS=false - TLS verification is disabled for {name} by explicit configuration"
        )

    errors.extend(node_errors)
    return ProxmoxNodeConfig(
        key=f"node{index}",
        index=index,
        name=name,
        host=host,
        port=port,
        api_node=api_node or name,
        token_id=token_id,
        token_secret=token_secret,
        verify_tls=verify_tls,
        ca_cert_path=ca_cert_path,
        timeout_seconds=timeout_seconds,
        errors=tuple(node_errors),
    )


@dataclass(frozen=True)
class Settings:
    """Complete validated configuration for one process."""

    app_host: str
    app_port: int
    usage: Thresholds
    temperature: Thresholds
    interval_cpu_mem: float
    interval_net: float
    interval_disk: float
    interval_temp: float
    interval_proxmox: float
    interval_disk_health: float
    interval_processes: float
    stream_push_interval: float
    disk_mounts: tuple[str, ...]
    disk_autodiscover: bool
    primary_disk_mount: str
    temp_enabled: bool
    lhm_http_url: str
    lhm_wmi_enabled: bool
    temp_probe_timeout: float
    sensor_backoff_seconds: float
    disk_health_enabled: bool
    stale_after_seconds: float
    dev_fake_proxmox: bool
    nodes: tuple[ProxmoxNodeConfig, ...]
    errors: tuple[str, ...]

    def node(self, key: str) -> ProxmoxNodeConfig | None:
        return next((n for n in self.nodes if n.key == key), None)

    def public_dict(self) -> dict[str, object]:
        """Configuration exposed to the browser. Contains no credentials."""
        return {
            "thresholds": {
                "usage": {"warning": self.usage.warning, "critical": self.usage.critical},
                "temperature": {
                    "warning": self.temperature.warning,
                    "critical": self.temperature.critical,
                },
            },
            "intervals_seconds": {
                "cpu_mem": self.interval_cpu_mem,
                "net": self.interval_net,
                "disk": self.interval_disk,
                "temperature": self.interval_temp,
                "proxmox": self.interval_proxmox,
                "disk_health": self.interval_disk_health,
                "processes": self.interval_processes,
                "stream_push": self.stream_push_interval,
            },
            "nodes": [n.public_dict() for n in self.nodes],
            "target_resolution": {"width": 1424, "height": 280},
            "dev_fake_proxmox": self.dev_fake_proxmox,
            "config_warnings": list(self.errors),
        }


def load_settings() -> Settings:
    """Build settings from the current environment."""
    errors: list[str] = []

    app_host = _raw("APP_HOST") or "127.0.0.1"
    app_port = _int("APP_PORT", 8765, errors, low=1, high=65535)

    warning = _float("THRESHOLD_WARNING", 70.0, errors, low=0.0, high=100.0)
    critical = _float("THRESHOLD_CRITICAL", 90.0, errors, low=0.0, high=100.0)
    if warning > critical:
        errors.append(
            f"THRESHOLD_WARNING ({warning}) above THRESHOLD_CRITICAL ({critical}); values swapped"
        )
        warning, critical = critical, warning

    # Temperature limits are separate from utilisation limits on purpose:
    # degrees Celsius and percent are not comparable scales.
    temp_warning = _float("TEMP_WARNING_C", 75.0, errors, low=0.0, high=150.0)
    temp_critical = _float("TEMP_CRITICAL_C", 90.0, errors, low=0.0, high=150.0)
    if temp_warning > temp_critical:
        errors.append(
            f"TEMP_WARNING_C ({temp_warning}) above TEMP_CRITICAL_C ({temp_critical}); values swapped"
        )
        temp_warning, temp_critical = temp_critical, temp_warning

    mounts_raw = _raw("DISK_MOUNTS")
    disk_mounts = tuple(m.strip() for m in mounts_raw.split(",") if m.strip()) if mounts_raw else ()

    nodes = tuple(_load_node(i, errors) for i in (1, 2))

    return Settings(
        app_host=app_host,
        app_port=app_port,
        usage=Thresholds(warning=warning, critical=critical),
        temperature=Thresholds(warning=temp_warning, critical=temp_critical),
        interval_cpu_mem=_float("INTERVAL_CPU_MEM", 1.5, errors, low=0.5, high=60.0),
        interval_net=_float("INTERVAL_NET", 1.5, errors, low=0.5, high=60.0),
        interval_disk=_float("INTERVAL_DISK", 5.0, errors, low=1.0, high=300.0),
        interval_temp=_float("INTERVAL_TEMP", 5.0, errors, low=1.0, high=300.0),
        interval_proxmox=_float("INTERVAL_PROXMOX", 4.0, errors, low=1.0, high=300.0),
        interval_disk_health=_float("INTERVAL_DISK_HEALTH", 60.0, errors, low=10.0, high=3600.0),
        interval_processes=_float("INTERVAL_PROCESSES", 10.0, errors, low=2.0, high=600.0),
        stream_push_interval=_float("STREAM_PUSH_INTERVAL", 1.0, errors, low=0.25, high=30.0),
        disk_mounts=disk_mounts,
        disk_autodiscover=_bool("DISK_AUTODISCOVER", True, errors),
        primary_disk_mount=_raw("PRIMARY_DISK_MOUNT") or os.environ.get("SystemDrive", "C:") + "\\",
        temp_enabled=_bool("TEMP_ENABLED", True, errors),
        lhm_http_url=_raw("LHM_HTTP_URL") or "http://127.0.0.1:8085/data.json",
        lhm_wmi_enabled=_bool("LHM_WMI_ENABLED", True, errors),
        temp_probe_timeout=_float("TEMP_PROBE_TIMEOUT_SECONDS", 2.0, errors, low=0.2, high=15.0),
        sensor_backoff_seconds=_float("SENSOR_BACKOFF_SECONDS", 60.0, errors, low=5.0, high=3600.0),
        disk_health_enabled=_bool("DISK_HEALTH_ENABLED", True, errors),
        stale_after_seconds=_float("STALE_AFTER_SECONDS", 15.0, errors, low=2.0, high=600.0),
        dev_fake_proxmox=_bool("DEV_FAKE_PROXMOX", False, errors),
        nodes=nodes,
        errors=tuple(errors),
    )


settings = load_settings()

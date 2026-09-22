"""Proxmox VE node collection over the official REST API.

One collector instance serves one node and owns one persistent HTTPS client,
so TLS is negotiated once rather than on every poll. Only per-node endpoints
are used (/nodes/{node}/...), never cluster-wide ones, so two independently
configured nodes cannot double-count the same resources.

Failure never propagates: an unreachable node returns its last good figures
marked stale with the timestamp of that success, and the loop keeps running.
Consecutive failures apply exponential backoff so a node that is switched off
is not polled at full rate.

Authentication uses an API token sent in the Authorization header. Token text
is scrubbed from any message that could reach a log or the browser.
"""

from __future__ import annotations

import ssl
import time
from datetime import datetime, timezone
from typing import Any

import httpx

from backend import config
from backend.config import ProxmoxNodeConfig, Settings


def _iso_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def percent_of(used: Any, total: Any) -> float | None:
    """Percentage of total, tolerating strings, None and zero denominators."""
    try:
        used_f = float(used)
        total_f = float(total)
    except (TypeError, ValueError):
        return None
    if total_f <= 0:
        return None
    return round(max(0.0, min(100.0, (used_f / total_f) * 100.0)), 1)


def _num(value: Any) -> float | None:
    """Coerce a JSON value to a float, or None when it is not numeric."""
    if isinstance(value, bool) or value is None:
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def _int_or_none(value: Any) -> int | None:
    number = _num(value)
    return int(number) if number is not None else None


def _sub(payload: Any, key: str) -> dict[str, Any]:
    """Return a nested mapping, or an empty one when the field is absent.

    Proxmox omits fields a token cannot see and adds fields between releases,
    so no subfield is assumed to exist.
    """
    if isinstance(payload, dict):
        value = payload.get(key)
        if isinstance(value, dict):
            return value
    return {}


class ProxmoxCollector:
    """Collects one Proxmox node, keeping its client and failure state."""

    def __init__(
        self,
        node: ProxmoxNodeConfig,
        settings: Settings,
        transport: httpx.AsyncBaseTransport | None = None,
    ) -> None:
        self.node = node
        self.settings = settings
        # A transport is supplied only by the tests, which drive the collector
        # against recorded Proxmox responses instead of a real node.
        self._transport = transport
        self._client: httpx.AsyncClient | None = None
        self._resolved_node: str | None = None
        self._failures = 0
        self._retry_after = 0.0

    # ------------------------------------------------------------------ setup

    def _redact(self, text: str) -> str:
        """Remove credential material from a message before it is surfaced."""
        cleaned = text
        for secret in (self.node.token_secret, self.node.token_id):
            if secret:
                cleaned = cleaned.replace(secret, "[redacted]")
        return cleaned

    async def _get_client(self) -> httpx.AsyncClient:
        if self._client is None or self._client.is_closed:
            kwargs: dict[str, Any] = {
                "base_url": self.node.base_url,
                "headers": self.node.auth_header(),
                "timeout": httpx.Timeout(self.node.timeout_seconds),
                "follow_redirects": False,
            }
            if self._transport is not None:
                kwargs["transport"] = self._transport
            else:
                kwargs["verify"] = self._tls_verify()
            self._client = httpx.AsyncClient(**kwargs)
        return self._client

    def _tls_verify(self) -> ssl.SSLContext | bool:
        """httpx 0.28 deprecates a CA path for verify, so a path becomes a
        context. Full verification stays on: chain, hostname or IP SAN, and
        Python's strict X.509 checks, which the Proxmox cluster CA passes."""
        option = self.node.verify_option()
        if isinstance(option, str):
            return ssl.create_default_context(cafile=option)
        return option

    async def aclose(self) -> None:
        if self._client is not None and not self._client.is_closed:
            await self._client.aclose()
        self._client = None

    async def _reset_client(self) -> None:
        """Drop the client so the next attempt reconnects from scratch."""
        try:
            await self.aclose()
        except Exception:
            self._client = None

    # ------------------------------------------------------------------ fetch

    async def _fetch(self, client: httpx.AsyncClient, path: str) -> Any:
        """GET one endpoint and return its data member.

        Raises PermissionError for 403 so a caller can treat an unprivileged
        subresource as simply absent, and httpx.HTTPStatusError for 401 so the
        node can be reported as an authentication failure.
        """
        response = await client.get(path)
        if response.status_code == 403:
            raise PermissionError(path)
        response.raise_for_status()
        try:
            payload = response.json()
        except ValueError as exc:
            raise ValueError(f"Response from {path} was not JSON") from exc
        if not isinstance(payload, dict) or "data" not in payload:
            raise ValueError(f"Response from {path} lacked a data member")
        return payload["data"]

    async def _resolve_node(self, client: httpx.AsyncClient) -> str:
        """Confirm the API node name, discovering it when the configured one
        does not exist. This allows a node to be renamed in Proxmox without a
        source change.

        Only a positive identification is cached. A guess (the configured name
        used because discovery failed) is retried on the next poll, so a node
        that was down when the dashboard started is still identified later.
        """
        if self._resolved_node:
            return self._resolved_node

        configured = self.node.api_node
        try:
            nodes = await self._fetch(client, "/nodes")
        except Exception:
            # Discovery is a convenience; the configured name is still tried.
            return configured

        names = [
            str(item.get("node"))
            for item in (nodes if isinstance(nodes, list) else [])
            if isinstance(item, dict) and item.get("node")
        ]
        resolved: str | None = None
        if configured in names:
            resolved = configured
        else:
            match = next((n for n in names if n.lower() == configured.lower()), None)
            if match:
                resolved = match
            elif len(names) == 1:
                resolved = names[0]
            elif len(names) > 1:
                # In a cluster /nodes lists every member whichever host answers,
                # so the node this host *is* comes from /cluster/status.
                resolved = await self._local_cluster_node(client)
        if resolved is None:
            return configured
        self._resolved_node = resolved
        return resolved

    async def _local_cluster_node(self, client: httpx.AsyncClient) -> str | None:
        """Name of the cluster member that answered, from its local flag.

        Used for identification only; no figures are read from the cluster
        endpoints, so two nodes still cannot double-count anything."""
        try:
            members = await self._fetch(client, "/cluster/status")
        except Exception:
            return None
        for item in members if isinstance(members, list) else []:
            if (
                isinstance(item, dict)
                and item.get("type") == "node"
                and item.get("local") in (1, True, "1")
                and item.get("name")
            ):
                return str(item["name"])
        return None

    # ---------------------------------------------------------------- results

    def _placeholder(self, status: str, message: str | None) -> dict[str, Any]:
        """A card with no figures at all, for a node that is not being polled."""
        return {
            "key": self.node.key,
            "name": self.node.name,
            "status": status,
            "api_node": None,
            "message": message,
            "last_attempt": _iso_now(),
            "last_success": None,
            "stale": False,
            "consecutive_failures": 0,
            "primary": {
                "cpu_percent": None,
                "memory_percent": None,
                "disk_percent": None,
                "disk_mount": None,
            },
        }

    def _carry_forward(self, previous: dict[str, Any] | None, status: str, message: str) -> dict[str, Any]:
        """Report a failure while preserving the last successful figures.

        The retained values keep their original last_success timestamp so the
        dashboard can show how old they are.
        """
        result: dict[str, Any] = {
            "key": self.node.key,
            "name": self.node.name,
            "status": status,
            "api_node": self._resolved_node,
            "message": message,
            "last_attempt": _iso_now(),
            "last_success": None,
            "stale": False,
            "consecutive_failures": self._failures,
            "primary": {
                "cpu_percent": None,
                "memory_percent": None,
                "disk_percent": None,
                "disk_mount": None,
            },
        }
        if previous and previous.get("last_success"):
            carried = {
                key: value
                for key, value in previous.items()
                if key
                not in {
                    "key",
                    "name",
                    "status",
                    "message",
                    "last_attempt",
                    "stale",
                    "consecutive_failures",
                }
            }
            result.update(carried)
            result["stale"] = True
        return result

    # ---------------------------------------------------------------- collect

    async def collect(self, previous: dict[str, Any] | None = None) -> dict[str, Any]:
        """Poll the node once. Never raises."""
        reason = self.node.unconfigured_reason
        if reason is not None:
            return self._placeholder(self.node.status_when_unreachable, reason)

        now = time.monotonic()
        if now < self._retry_after:
            wait = int(self._retry_after - now)
            return self._carry_forward(
                previous, config.STATUS_OFFLINE, f"Unreachable; next retry in {wait}s"
            )

        try:
            client = await self._get_client()
            node_name = await self._resolve_node(client)
            status = await self._fetch(client, f"/nodes/{node_name}/status")
            if not isinstance(status, dict):
                raise ValueError("Node status was not an object")

            result = self._build(status, node_name)
            result.update(await self._collect_optional(client, node_name, previous))

            self._failures = 0
            self._retry_after = 0.0
            return result

        except httpx.HTTPStatusError as exc:
            code = exc.response.status_code
            self._register_failure()
            if code == 401:
                # A rejected token will keep being rejected; the client is kept
                # but the state is distinct from an unreachable host.
                return self._carry_forward(
                    previous,
                    config.STATUS_AUTH_ERROR,
                    "Authentication failed (401) - check token id, secret and permissions",
                )
            if code == 403:
                return self._carry_forward(
                    previous,
                    config.STATUS_AUTH_ERROR,
                    "Token lacks permission for /nodes status (403) - grant PVEAuditor",
                )
            return self._carry_forward(previous, config.STATUS_OFFLINE, f"HTTP {code} from node")

        except PermissionError:
            self._register_failure()
            return self._carry_forward(
                previous,
                config.STATUS_AUTH_ERROR,
                "Token lacks permission for node status (403) - grant PVEAuditor",
            )

        except (httpx.ConnectError, httpx.ConnectTimeout) as exc:
            self._register_failure()
            await self._reset_client()
            return self._carry_forward(
                previous, config.STATUS_OFFLINE, f"Connection failed: {self._redact(str(exc)) or 'unreachable'}"
            )

        except httpx.TimeoutException:
            self._register_failure()
            return self._carry_forward(
                previous,
                config.STATUS_OFFLINE,
                f"Timed out after {self.node.timeout_seconds:g}s",
            )

        except ValueError as exc:
            # Malformed or truncated payload: reachable but unusable.
            self._register_failure()
            return self._carry_forward(
                previous, config.STATUS_OFFLINE, f"Invalid API response: {self._redact(str(exc))}"
            )

        except Exception as exc:
            self._register_failure()
            await self._reset_client()
            return self._carry_forward(
                previous, config.STATUS_OFFLINE, f"{type(exc).__name__}: {self._redact(str(exc))}"
            )

    def _register_failure(self) -> None:
        """Grow the retry delay geometrically, capped at one minute."""
        self._failures += 1
        delay = min(60.0, self.settings.interval_proxmox * (2 ** min(self._failures - 1, 5)))
        self._retry_after = time.monotonic() + delay

    # ------------------------------------------------------------------ parse

    def _build(self, status: dict[str, Any], node_name: str) -> dict[str, Any]:
        """Map a /nodes/{node}/status payload onto the dashboard shape."""
        cpu_fraction = _num(status.get("cpu"))
        cpu_percent = round(max(0.0, min(100.0, cpu_fraction * 100.0)), 1) if cpu_fraction is not None else None

        memory = _sub(status, "memory")
        memory_total = _int_or_none(memory.get("total"))
        memory_used = _int_or_none(memory.get("used"))
        memory_percent = percent_of(memory_used, memory_total)

        rootfs = _sub(status, "rootfs")
        rootfs_total = _int_or_none(rootfs.get("total"))
        rootfs_used = _int_or_none(rootfs.get("used"))
        rootfs_percent = percent_of(rootfs_used, rootfs_total)

        swap = _sub(status, "swap")
        swap_percent = percent_of(swap.get("used"), swap.get("total"))

        load: dict[str, Any] = {"min1": None, "min5": None, "min15": None, "emulated": False}
        raw_load = status.get("loadavg")
        if isinstance(raw_load, list):
            for key, value in zip(("min1", "min5", "min15"), raw_load):
                number = _num(value)
                load[key] = round(number, 2) if number is not None else None

        cpuinfo = _sub(status, "cpuinfo")
        timestamp = _iso_now()

        return {
            "key": self.node.key,
            "name": self.node.name,
            "status": config.STATUS_ONLINE,
            "api_node": node_name,
            "message": None,
            "last_attempt": timestamp,
            "last_success": timestamp,
            "stale": False,
            "consecutive_failures": 0,
            "primary": {
                "cpu_percent": cpu_percent,
                "memory_percent": memory_percent,
                "disk_percent": rootfs_percent,
                "disk_mount": "rootfs",
            },
            "cpu_percent": cpu_percent,
            "cpu_count": _int_or_none(cpuinfo.get("cpus")),
            "memory_total_bytes": memory_total,
            "memory_used_bytes": memory_used,
            "memory_percent": memory_percent,
            "rootfs_total_bytes": rootfs_total,
            "rootfs_used_bytes": rootfs_used,
            "rootfs_percent": rootfs_percent,
            "swap_percent": swap_percent,
            "uptime_seconds": _num(status.get("uptime")),
            "load": load,
            "pve_version": status.get("pveversion") if isinstance(status.get("pveversion"), str) else None,
            "kernel": status.get("kversion") if isinstance(status.get("kversion"), str) else None,
            # Proxmox exposes no documented temperature endpoint, so no value
            # is reported rather than one inferred from an unrelated field.
            "temperature": {
                "cpu_celsius": None,
                "source": None,
                "available": False,
                "detail": "Not exposed by the Proxmox VE API",
            },
        }

    async def _collect_optional(
        self, client: httpx.AsyncClient, node_name: str, previous: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        """Storage and guest inventory, each optional on token permissions.

        A section that fails for a transient reason (a node busy enough that
        its storage query times out) keeps the last value it read, so the card
        does not flicker between figures and blanks. A permission refusal is
        reported as such and never masked by an old value."""
        extra: dict[str, Any] = {}
        before = previous if isinstance(previous, dict) and previous.get("last_success") else {}

        def carried(*keys: str) -> dict[str, Any] | None:
            if all(before.get(k) is not None for k in keys):
                return {k: before[k] for k in keys}
            return None

        try:
            storage = await self._fetch(client, f"/nodes/{node_name}/storage")
            entries: list[dict[str, Any]] = []
            total_sum = 0
            avail_sum = 0
            seen: set[str] = set()
            for item in storage if isinstance(storage, list) else []:
                if not isinstance(item, dict):
                    continue
                name = item.get("storage")
                total = _int_or_none(item.get("total"))
                used = _int_or_none(item.get("used"))
                avail = _int_or_none(item.get("avail"))
                entries.append(
                    {
                        "name": str(name) if name else None,
                        "type": str(item.get("type")) if item.get("type") else None,
                        "total_bytes": total,
                        "used_bytes": used,
                        "available_bytes": avail,
                        "percent": percent_of(used, total),
                        "enabled": bool(item.get("enabled", True)),
                    }
                )
                # Shared storage is listed once per node; a repeated name on
                # the same node is not added to the totals twice.
                key = str(name)
                if key not in seen:
                    seen.add(key)
                    total_sum += total or 0
                    avail_sum += avail or 0
            extra["storage"] = entries
            extra["storage_total_bytes"] = total_sum or None
            extra["storage_available_bytes"] = avail_sum or None
        except PermissionError:
            extra["storage"] = []
        except Exception:
            extra.update(
                carried("storage", "storage_total_bytes", "storage_available_bytes") or {"storage": []}
            )

        for path, key in (("qemu", "vms"), ("lxc", "containers")):
            try:
                guests = await self._fetch(client, f"/nodes/{node_name}/{path}")
                items = [g for g in (guests if isinstance(guests, list) else []) if isinstance(g, dict)]
                extra[key] = {
                    "total": len(items),
                    "running": sum(1 for g in items if g.get("status") == "running"),
                    "permitted": True,
                }
            except PermissionError:
                extra[key] = {"total": None, "running": None, "permitted": False}
            except Exception:
                old = carried(key)
                extra[key] = old[key] if old else {"total": None, "running": None, "permitted": True}

        return extra

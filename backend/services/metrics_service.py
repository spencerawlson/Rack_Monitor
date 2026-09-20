"""Background collection and snapshot distribution.

Each metric family runs in its own loop at its own interval, so fast values
(CPU, memory, network) are sampled often while expensive ones (filesystems,
temperature probing, drive health, Proxmox) are sampled rarely. A loop is
strictly sequential: the next sample starts only after the previous one has
finished, which is what prevents overlapping requests and keeps derived rates
correct. Blocking psutil and PowerShell work is pushed to worker threads so
the event loop stays responsive while a source is slow or unavailable.

A loop that raises is recorded and retried on its next tick; it is never
allowed to terminate, because one failing source must not stop the others.
"""

from __future__ import annotations

import asyncio
import copy
import time
from datetime import datetime, timezone
from typing import Any, Awaitable, Callable

from backend import config
from backend.collectors.health_collector import HealthCollector
from backend.collectors.proxmox_collector import ProxmoxCollector
from backend.collectors.temperature_collector import TemperatureCollector
from backend.collectors.windows_collector import WindowsCollector, percent_of
from backend.config import Settings

VERSION = "1.0.0"


def _iso_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


class _CollectorState:
    """Liveness bookkeeping for one loop."""

    def __init__(self) -> None:
        # Staleness is measured from creation until the first success, so a
        # loop whose opening run is still in flight is starting, not late.
        self.created_monotonic = time.monotonic()
        self.last_attempt: str | None = None
        self.last_success: str | None = None
        self.last_success_monotonic: float | None = None
        self.last_error: str | None = None
        self.runs = 0
        self.failures = 0

    def to_dict(self, stale_after: float, interval: float = 0.0) -> dict[str, Any]:
        # A loop is late only relative to its own cadence: a 60 second drive
        # health poll is not stale five seconds after its last success.
        threshold = max(stale_after, interval * 3.0)
        reference = (
            self.last_success_monotonic
            if self.last_success_monotonic is not None
            else self.created_monotonic
        )
        stale = (time.monotonic() - reference) > threshold
        return {
            "last_attempt": self.last_attempt,
            "last_success": self.last_success,
            "last_error": self.last_error,
            "stale": stale,
            "runs": self.runs,
            "failures": self.failures,
        }


class MetricsService:
    """Owns the collectors, the current snapshot and the stream subscribers."""

    def __init__(self, settings: Settings | None = None) -> None:
        self.settings = settings or config.settings
        self.windows = WindowsCollector(self.settings)
        self.temperature = TemperatureCollector(self.settings)
        self.health = HealthCollector(self.settings)
        self.proxmox: dict[str, ProxmoxCollector] = {
            node.key: ProxmoxCollector(node, self.settings) for node in self.settings.nodes
        }

        self._lock = asyncio.Lock()
        self._tasks: list[asyncio.Task[None]] = []
        self._subscribers: set[asyncio.Queue[dict[str, Any]]] = set()
        self._states: dict[str, _CollectorState] = {
            name: _CollectorState()
            for name in (
                "cpu_mem",
                "net",
                "disk",
                "processes",
                "temperature",
                "disk_health",
                "proxmox",
            )
        }
        # Each loop's cadence, used to judge whether that loop is overdue.
        self._intervals: dict[str, float] = {
            "cpu_mem": self.settings.interval_cpu_mem,
            "net": self.settings.interval_net,
            "disk": self.settings.interval_disk,
            "processes": self.settings.interval_processes,
            "temperature": self.settings.interval_temp,
            "disk_health": self.settings.interval_disk_health,
            "proxmox": self.settings.interval_proxmox,
        }
        self._started_monotonic: float | None = None
        self._started_at: str | None = None
        self._windows: dict[str, Any] = self._initial_windows()
        self._nodes: dict[str, dict[str, Any]] = {
            node.key: {
                "key": node.key,
                "name": node.name,
                "status": node.status_when_unreachable,
                "message": node.unconfigured_reason,
                "last_attempt": None,
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
            for node in self.settings.nodes
        }

    # ---------------------------------------------------------------- initial

    @staticmethod
    def _initial_windows() -> dict[str, Any]:
        """A snapshot with no readings yet. Absent values stay None so the
        dashboard shows an unavailable state instead of a convincing zero."""
        return {
            "system": {},
            "cpu": {"per_core": [], "load": {}},
            "memory": {},
            "swap": {},
            "filesystems": [],
            "disk_io": {"per_disk": []},
            "network": {"per_interface": [], "connectivity": "UNKNOWN"},
            "processes": {},
            "temperature": {
                "cpu_celsius": None,
                "source": None,
                "available": False,
                "detail": "Not probed yet",
            },
            "disk_health": {"available": False, "detail": "Not probed yet", "disks": []},
            "primary": {
                "cpu_percent": None,
                "memory_percent": None,
                "disk_percent": None,
                "disk_mount": None,
            },
        }

    # ------------------------------------------------------------------ start

    async def start(self) -> None:
        if self._tasks:
            return
        self._started_monotonic = time.monotonic()
        self._started_at = _iso_now()

        # One synchronous pass over the cheap families before the loops begin,
        # so the first request served already carries readings instead of an
        # empty snapshot. CPU percentages still need a second sample and
        # appear on the next tick.
        for name, step in (
            ("cpu_mem", self._step_cpu_mem),
            ("net", self._step_net),
            ("disk", self._step_disk),
        ):
            try:
                await step()
                state = self._states[name]
                state.last_attempt = state.last_success = _iso_now()
                state.last_success_monotonic = time.monotonic()
                state.runs += 1
            except Exception as exc:
                self._states[name].last_error = f"{type(exc).__name__}: {exc}"

        loops: list[tuple[str, float, Callable[[], Awaitable[None]]]] = [
            ("cpu_mem", self.settings.interval_cpu_mem, self._step_cpu_mem),
            ("net", self.settings.interval_net, self._step_net),
            ("disk", self.settings.interval_disk, self._step_disk),
            ("processes", self.settings.interval_processes, self._step_processes),
            ("temperature", self.settings.interval_temp, self._step_temperature),
            ("disk_health", self.settings.interval_disk_health, self._step_disk_health),
            ("proxmox", self.settings.interval_proxmox, self._step_proxmox),
        ]
        for name, interval, step in loops:
            self._tasks.append(
                asyncio.create_task(self._run_loop(name, interval, step), name=f"plh-{name}")
            )
        self._tasks.append(asyncio.create_task(self._broadcast_loop(), name="plh-broadcast"))

    async def stop(self) -> None:
        for task in self._tasks:
            task.cancel()
        for task in self._tasks:
            try:
                await task
            except (asyncio.CancelledError, Exception):
                pass
        self._tasks.clear()
        for collector in self.proxmox.values():
            try:
                await collector.aclose()
            except Exception:
                pass

    async def _run_loop(
        self, name: str, interval: float, step: Callable[[], Awaitable[None]]
    ) -> None:
        """Run one collection step forever, pacing to the configured interval."""
        state = self._states[name]
        while True:
            started = time.monotonic()
            state.last_attempt = _iso_now()
            state.runs += 1
            try:
                await step()
                state.last_success = state.last_attempt
                state.last_success_monotonic = time.monotonic()
                state.last_error = None
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                state.failures += 1
                state.last_error = f"{type(exc).__name__}: {exc}"
            # Sleeping the remainder keeps the cadence steady and guarantees a
            # gap even when one sample took longer than its whole interval.
            elapsed = time.monotonic() - started
            await asyncio.sleep(max(0.25, interval - elapsed))

    # ------------------------------------------------------------------ steps

    async def _step_cpu_mem(self) -> None:
        cpu = await asyncio.to_thread(self.windows.sample_cpu)
        memory = await asyncio.to_thread(self.windows.sample_memory)
        swap = await asyncio.to_thread(self.windows.sample_swap)
        system = await asyncio.to_thread(self.windows.system_info)
        async with self._lock:
            self._windows["cpu"] = cpu
            self._windows["memory"] = memory
            self._windows["swap"] = swap
            self._windows["system"] = system
            self._windows["primary"]["cpu_percent"] = cpu.get("percent")
            self._windows["primary"]["memory_percent"] = memory.get("percent")

    async def _step_net(self) -> None:
        network = await asyncio.to_thread(self.windows.sample_network)
        async with self._lock:
            self._windows["network"] = network

    async def _step_processes(self) -> None:
        # Walking every process is the most expensive sample taken, so it runs
        # on its own slow loop instead of delaying disk throughput rates.
        processes = await asyncio.to_thread(self.windows.sample_processes)
        async with self._lock:
            self._windows["processes"] = processes

    async def _step_disk(self) -> None:
        filesystems = await asyncio.to_thread(self.windows.sample_filesystems)
        disk_io = await asyncio.to_thread(self.windows.sample_disk_io)

        # The donut tracks the primary mount; other mounts stay in the detail
        # view so one figure is never a blend of several disks.
        primary_mount = self.settings.primary_disk_mount
        chosen = next(
            (fs for fs in filesystems if fs.get("mount") == primary_mount and fs.get("present")),
            None,
        )
        if chosen is None:
            chosen = next((fs for fs in filesystems if fs.get("present")), None)

        async with self._lock:
            self._windows["filesystems"] = filesystems
            self._windows["disk_io"] = disk_io
            self._windows["primary"]["disk_percent"] = chosen.get("percent") if chosen else None
            self._windows["primary"]["disk_mount"] = chosen.get("mount") if chosen else None

    async def _step_temperature(self) -> None:
        reading = await self.temperature.read()
        async with self._lock:
            self._windows["temperature"] = reading

    async def _step_disk_health(self) -> None:
        reading = await self.health.disk_health()
        async with self._lock:
            self._windows["disk_health"] = reading

    async def _step_proxmox(self) -> None:
        if not self.proxmox:
            return
        keys = list(self.proxmox)
        async with self._lock:
            previous = {key: copy.deepcopy(self._nodes.get(key)) for key in keys}

        # Nodes are polled concurrently, so one unreachable node delays the
        # cycle by its own timeout rather than by the sum of all timeouts.
        results = await asyncio.gather(
            *(self.proxmox[key].collect(previous.get(key)) for key in keys),
            return_exceptions=True,
        )

        async with self._lock:
            for key, result in zip(keys, results):
                if isinstance(result, BaseException):
                    node = self.settings.node(key)
                    self._nodes[key] = {
                        "key": key,
                        "name": node.name if node else key,
                        "status": config.STATUS_OFFLINE,
                        "message": f"Collector error: {type(result).__name__}",
                        "last_attempt": _iso_now(),
                        "last_success": self._nodes.get(key, {}).get("last_success"),
                        "stale": bool(self._nodes.get(key, {}).get("last_success")),
                        "consecutive_failures": 0,
                        "primary": {
                            "cpu_percent": None,
                            "memory_percent": None,
                            "disk_percent": None,
                            "disk_mount": None,
                        },
                    }
                else:
                    self._nodes[key] = result

            if self.settings.dev_fake_proxmox:
                self._apply_dev_mock()

    def _apply_dev_mock(self) -> None:
        """Development aid: fill unconfigured nodes with obviously fake values.

        The status is DEV_MOCK and the message says so, so a screenshot taken
        in this mode cannot be mistaken for real node data.
        """
        for index, (key, node) in enumerate(self._nodes.items()):
            if node.get("status") in {config.STATUS_UNCONFIGURED, config.STATUS_CONFIG_ERROR}:
                base = 20.0 + index * 7
                self._nodes[key] = {
                    **node,
                    "status": "DEV_MOCK",
                    "message": "Synthetic development data - not a real node",
                    "last_attempt": _iso_now(),
                    "last_success": _iso_now(),
                    "stale": False,
                    "primary": {
                        "cpu_percent": base,
                        "memory_percent": base + 15,
                        "disk_percent": base + 25,
                        "disk_mount": "rootfs",
                    },
                    "cpu_percent": base,
                    "memory_percent": base + 15,
                    "rootfs_percent": base + 25,
                    "uptime_seconds": 86400 * (index + 3),
                    "vms": {"running": 2, "total": 3, "permitted": True},
                    "containers": {"running": 1, "total": 1, "permitted": True},
                }

    # --------------------------------------------------------------- snapshot

    def _service_state(self) -> dict[str, Any]:
        uptime = None
        if self._started_monotonic is not None:
            uptime = round(time.monotonic() - self._started_monotonic, 1)
        collectors = {
            name: state.to_dict(self.settings.stale_after_seconds, self._intervals.get(name, 0.0))
            for name, state in self._states.items()
        }
        # The service is degraded, not down, while any loop is stale: the rest
        # of the dashboard keeps updating.
        degraded = any(entry["stale"] for entry in collectors.values())
        return {
            "status": "DEGRADED" if degraded else "RUNNING",
            "started_at": self._started_at,
            "uptime_seconds": uptime,
            "version": VERSION,
            "collectors": collectors,
            "config_warnings": list(self.settings.errors),
        }

    async def get_snapshot(self) -> dict[str, Any]:
        """Current readings. Safe to call at any time; performs no collection."""
        async with self._lock:
            return {
                "generated_at": _iso_now(),
                "service": self._service_state(),
                "windows": copy.deepcopy(self._windows),
                "proxmox": copy.deepcopy(self._nodes),
            }

    # ----------------------------------------------------------------- stream

    async def subscribe(self) -> asyncio.Queue[dict[str, Any]]:
        queue: asyncio.Queue[dict[str, Any]] = asyncio.Queue(maxsize=1)
        self._subscribers.add(queue)
        queue.put_nowait(await self.get_snapshot())
        return queue

    def unsubscribe(self, queue: asyncio.Queue[dict[str, Any]]) -> None:
        self._subscribers.discard(queue)

    async def _broadcast_loop(self) -> None:
        """Push the snapshot to stream clients at the configured cadence."""
        while True:
            await asyncio.sleep(self.settings.stream_push_interval)
            if not self._subscribers:
                continue
            snapshot = await self.get_snapshot()
            for queue in list(self._subscribers):
                # A client that has not read its previous frame gets the newer
                # one instead; frames are never queued up behind a slow reader.
                if queue.full():
                    try:
                        queue.get_nowait()
                    except asyncio.QueueEmpty:
                        pass
                try:
                    queue.put_nowait(snapshot)
                except asyncio.QueueFull:
                    pass


__all__ = ["MetricsService", "VERSION", "percent_of"]

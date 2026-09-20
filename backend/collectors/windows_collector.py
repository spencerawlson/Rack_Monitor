"""Windows host metric collection via psutil.

Each metric family has its own sample method so that the service layer can
poll fast-moving values often and slow-moving ones rarely. Rates are derived
from counter deltas measured against a monotonic clock; a negative delta means
the counter was reset and yields 0 rather than a negative rate.

No network probe of any external host is performed: connectivity is judged
from local link state, which keeps the application usable offline.
"""

from __future__ import annotations

import platform
import socket
import time
from typing import Any

import psutil

from backend.config import Settings


def percent_of(used: float | int | None, total: float | int | None) -> float | None:
    """Percentage of total, or None when the denominator is unusable.

    Results are clamped to 0..100 so a transient counter anomaly cannot push a
    donut chart past full scale.
    """
    if used is None or total is None:
        return None
    try:
        total_f = float(total)
        used_f = float(used)
    except (TypeError, ValueError):
        return None
    if total_f <= 0:
        return None
    return round(max(0.0, min(100.0, (used_f / total_f) * 100.0)), 1)


class _RateTracker:
    """Converts monotonically increasing counters into per-second rates."""

    def __init__(self) -> None:
        self._previous: dict[str, tuple[float, float]] = {}

    def rate(self, key: str, value: float | None) -> float | None:
        """Return the per-second change for key, or None on the first sample."""
        if value is None:
            return None
        now = time.monotonic()
        previous = self._previous.get(key)
        self._previous[key] = (float(value), now)
        if previous is None:
            return None
        prev_value, prev_ts = previous
        elapsed = now - prev_ts
        if elapsed <= 0:
            return None
        delta = float(value) - prev_value
        if delta < 0:
            # A counter reset (adapter restart, disk reattach) is not a rate.
            return 0.0
        return delta / elapsed


class WindowsCollector:
    """Collects Windows host metrics. One instance owns all rate state."""

    # A delta shorter than this is too small to divide by without amplifying
    # scheduler jitter into a meaningless percentage.
    MIN_CPU_DELTA_SECONDS = 0.2

    def __init__(self, settings: Settings) -> None:
        self.settings = settings
        self._rates = _RateTracker()
        self._system_cache: dict[str, Any] | None = None
        # CPU percentages are derived from cpu_times deltas held on this
        # instance rather than from psutil.cpu_percent. psutil keeps its
        # comparison state per calling thread, and every sample here runs on a
        # different worker thread, which would make every reading 0.0.
        self._prev_cpu_times: tuple[Any, float] | None = None
        self._prev_cpu_times_percpu: tuple[list[Any], float] | None = None
        self._prime_cpu()

    def _prime_cpu(self) -> None:
        """Take the baseline CPU sample that the first reading compares against."""
        try:
            now = time.monotonic()
            self._prev_cpu_times = (psutil.cpu_times(), now)
            self._prev_cpu_times_percpu = (psutil.cpu_times(percpu=True), now)
        except Exception:
            self._prev_cpu_times = None
            self._prev_cpu_times_percpu = None

    @staticmethod
    def _times_breakdown(previous: Any, current: Any) -> dict[str, float] | None:
        """Percentage of wall time in each CPU state between two samples."""
        fields = [f for f in current._fields if isinstance(getattr(current, f, None), (int, float))]
        deltas = {f: max(0.0, float(getattr(current, f)) - float(getattr(previous, f, 0.0))) for f in fields}
        total = sum(deltas.values())
        if total <= 0:
            return None
        breakdown = {f: round((value / total) * 100.0, 1) for f, value in deltas.items()}
        idle = breakdown.get("idle", 0.0)
        breakdown["busy"] = round(max(0.0, min(100.0, 100.0 - idle)), 1)
        return breakdown

    # ---------------------------------------------------------------- system

    def system_info(self) -> dict[str, Any]:
        """Host identity and uptime. Static parts are cached after first use."""
        if self._system_cache is None:
            cpu_name = platform.processor() or None
            self._system_cache = {
                "hostname": socket.gethostname(),
                "os_name": platform.system(),
                "os_version": platform.release(),
                "platform": platform.machine(),
                "cpu_name": cpu_name,
                "boot_time": psutil.boot_time(),
            }
        info = dict(self._system_cache)
        boot = info.get("boot_time")
        info["uptime_seconds"] = max(0.0, time.time() - boot) if boot else None
        return info

    # ------------------------------------------------------------------- cpu

    def sample_cpu(self) -> dict[str, Any]:
        overall: float | None = None
        breakdown: dict[str, float | None] = {
            "user": None,
            "system": None,
            "idle": None,
            "interrupt": None,
            "dpc": None,
        }
        per_core: list[dict[str, Any]] = []

        now = time.monotonic()
        try:
            current = psutil.cpu_times()
            previous = self._prev_cpu_times
            if previous is not None and (now - previous[1]) >= self.MIN_CPU_DELTA_SECONDS:
                computed = self._times_breakdown(previous[0], current)
                if computed is not None:
                    overall = computed["busy"]
                    for field in list(breakdown):
                        if field in computed:
                            breakdown[field] = computed[field]
                self._prev_cpu_times = (current, now)
            elif previous is None:
                self._prev_cpu_times = (current, now)
        except Exception:
            pass

        try:
            current_per_cpu = psutil.cpu_times(percpu=True)
            previous_per_cpu = self._prev_cpu_times_percpu
            if previous_per_cpu is not None and (now - previous_per_cpu[1]) >= self.MIN_CPU_DELTA_SECONDS:
                for index, core_times in enumerate(current_per_cpu):
                    if index >= len(previous_per_cpu[0]):
                        break
                    computed = self._times_breakdown(previous_per_cpu[0][index], core_times)
                    per_core.append(
                        {"core": index, "percent": computed["busy"] if computed else None}
                    )
                self._prev_cpu_times_percpu = (current_per_cpu, now)
            elif previous_per_cpu is None:
                self._prev_cpu_times_percpu = (current_per_cpu, now)
        except Exception:
            pass

        freq_current = freq_min = freq_max = None
        try:
            freq = psutil.cpu_freq()
            if freq is not None:
                freq_current = float(freq.current) or None
                freq_min = float(freq.min) or None
                freq_max = float(freq.max) or None
        except Exception:
            pass

        ctx_rate = int_rate = None
        try:
            stats = psutil.cpu_stats()
            ctx_rate = self._rates.rate("ctx_switches", stats.ctx_switches)
            int_rate = self._rates.rate("interrupts", stats.interrupts)
        except Exception:
            pass

        # getloadavg is emulated on Windows and reports 0.0 until its sampling
        # thread has been running long enough to produce a figure.
        load: dict[str, Any] = {"min1": None, "min5": None, "min15": None, "emulated": True}
        try:
            one, five, fifteen = psutil.getloadavg()
            load.update({"min1": round(one, 2), "min5": round(five, 2), "min15": round(fifteen, 2)})
        except Exception:
            pass

        return {
            "percent": overall,
            **breakdown,
            "cores_physical": psutil.cpu_count(logical=False),
            "cores_logical": psutil.cpu_count(logical=True),
            "freq_current_mhz": round(freq_current, 1) if freq_current else None,
            "freq_min_mhz": round(freq_min, 1) if freq_min else None,
            "freq_max_mhz": round(freq_max, 1) if freq_max else None,
            "ctx_switches_per_sec": round(ctx_rate) if ctx_rate is not None else None,
            "interrupts_per_sec": round(int_rate) if int_rate is not None else None,
            "per_core": per_core,
            "load": load,
        }

    # ---------------------------------------------------------------- memory

    def sample_memory(self) -> dict[str, Any]:
        vm = psutil.virtual_memory()
        return {
            "total_bytes": int(vm.total),
            "used_bytes": int(vm.used),
            "available_bytes": int(vm.available),
            "free_bytes": int(getattr(vm, "free", vm.available)),
            "percent": round(float(vm.percent), 1),
        }

    def sample_swap(self) -> dict[str, Any]:
        try:
            sw = psutil.swap_memory()
        except Exception:
            return {"total_bytes": None, "used_bytes": None, "free_bytes": None, "percent": None}
        return {
            "total_bytes": int(sw.total),
            "used_bytes": int(sw.used),
            "free_bytes": int(sw.free),
            "percent": round(float(sw.percent), 1),
        }

    # ------------------------------------------------------------- disk (fs)

    def _candidate_mounts(self) -> list[str]:
        """Mounts to report: explicitly configured ones plus discovered ones."""
        mounts: list[str] = list(self.settings.disk_mounts)
        if self.settings.disk_autodiscover:
            try:
                for part in psutil.disk_partitions(all=False):
                    if "cdrom" in (part.opts or "").lower() or not part.fstype:
                        continue
                    if part.mountpoint not in mounts:
                        mounts.append(part.mountpoint)
            except Exception:
                pass
        primary = self.settings.primary_disk_mount
        if primary and primary not in mounts:
            mounts.insert(0, primary)
        return mounts

    def sample_filesystems(self) -> list[dict[str, Any]]:
        """Capacity per mount point. A configured but absent drive is kept
        visible with present=False instead of disappearing from the dashboard."""
        try:
            partitions = {p.mountpoint: p for p in psutil.disk_partitions(all=False)}
        except Exception:
            partitions = {}

        results: list[dict[str, Any]] = []
        for mount in self._candidate_mounts():
            part = partitions.get(mount)
            entry: dict[str, Any] = {
                "mount": mount,
                "device": getattr(part, "device", None),
                "fstype": getattr(part, "fstype", None),
                "options": getattr(part, "opts", None),
                "total_bytes": None,
                "used_bytes": None,
                "free_bytes": None,
                "percent": None,
                "present": False,
                "error": None,
            }
            try:
                usage = psutil.disk_usage(mount)
            except (OSError, PermissionError) as exc:
                # Typical for a detached external SSD or an empty card reader.
                entry["error"] = type(exc).__name__
                results.append(entry)
                continue
            entry.update(
                {
                    "total_bytes": int(usage.total),
                    "used_bytes": int(usage.used),
                    "free_bytes": int(usage.free),
                    "percent": percent_of(usage.used, usage.total),
                    "present": True,
                }
            )
            results.append(entry)
        return results

    def sample_disk_io(self) -> dict[str, Any]:
        """Throughput, which is activity and not the same thing as capacity."""
        result: dict[str, Any] = {
            "read_bytes_per_sec": None,
            "write_bytes_per_sec": None,
            "read_count_per_sec": None,
            "write_count_per_sec": None,
            "per_disk": [],
        }
        try:
            totals = psutil.disk_io_counters()
        except Exception:
            totals = None
        if totals is not None:
            result["read_bytes_per_sec"] = self._rates.rate("io_read_bytes", totals.read_bytes)
            result["write_bytes_per_sec"] = self._rates.rate("io_write_bytes", totals.write_bytes)
            result["read_count_per_sec"] = self._rates.rate("io_read_count", totals.read_count)
            result["write_count_per_sec"] = self._rates.rate("io_write_count", totals.write_count)

        try:
            per_disk = psutil.disk_io_counters(perdisk=True) or {}
        except Exception:
            per_disk = {}
        for name, counters in sorted(per_disk.items()):
            result["per_disk"].append(
                {
                    "name": name,
                    "read_bytes_per_sec": self._rates.rate(f"io_{name}_read", counters.read_bytes),
                    "write_bytes_per_sec": self._rates.rate(f"io_{name}_write", counters.write_bytes),
                }
            )
        return result

    # --------------------------------------------------------------- network

    @staticmethod
    def _ipv4_for(interface: str) -> tuple[str | None, int | None]:
        """First IPv4 address and prefix length for an interface, if any."""
        try:
            for addr in psutil.net_if_addrs().get(interface, []):
                if addr.family == socket.AF_INET:
                    cidr = None
                    if addr.netmask:
                        cidr = sum(bin(int(octet)).count("1") for octet in addr.netmask.split("."))
                    return addr.address, cidr
        except Exception:
            pass
        return None, None

    def sample_network(self) -> dict[str, Any]:
        try:
            per_nic = psutil.net_io_counters(pernic=True) or {}
            stats = psutil.net_if_stats() or {}
        except Exception:
            return {"connectivity": "UNKNOWN", "per_interface": []}

        interfaces: list[dict[str, Any]] = []
        for name, counters in sorted(per_nic.items()):
            nic = stats.get(name)
            interfaces.append(
                {
                    "name": name,
                    "up": bool(nic.isup) if nic else None,
                    "speed_mbps": int(nic.speed) if nic and nic.speed else None,
                    "download_bytes_per_sec": self._rates.rate(f"net_{name}_recv", counters.bytes_recv),
                    "upload_bytes_per_sec": self._rates.rate(f"net_{name}_sent", counters.bytes_sent),
                    "bytes_recv": int(counters.bytes_recv),
                    "bytes_sent": int(counters.bytes_sent),
                }
            )

        # The active interface is the up, non-loopback adapter holding an IPv4
        # address and moving the most traffic; ties fall back to link speed.
        best: dict[str, Any] | None = None
        best_score = (-1.0, -1)
        for entry in interfaces:
            if not entry["up"] or entry["name"].lower().startswith("loopback"):
                continue
            address, _ = self._ipv4_for(entry["name"])
            if address is None:
                continue
            traffic = (entry["download_bytes_per_sec"] or 0.0) + (entry["upload_bytes_per_sec"] or 0.0)
            score = (traffic, entry["speed_mbps"] or 0)
            if score > best_score:
                best_score = score
                best = entry

        result: dict[str, Any] = {
            "interface": None,
            "ip_address": None,
            "ip_mask_cidr": None,
            "link_up": None,
            "speed_mbps": None,
            "download_bytes_per_sec": None,
            "upload_bytes_per_sec": None,
            "total_bytes_recv": None,
            "total_bytes_sent": None,
            "connectivity": "DOWN",
            "per_interface": interfaces,
        }
        if best is not None:
            address, cidr = self._ipv4_for(best["name"])
            result.update(
                {
                    "interface": best["name"],
                    "ip_address": address,
                    "ip_mask_cidr": cidr,
                    "link_up": True,
                    "speed_mbps": best["speed_mbps"],
                    "download_bytes_per_sec": best["download_bytes_per_sec"],
                    "upload_bytes_per_sec": best["upload_bytes_per_sec"],
                    "total_bytes_recv": best["bytes_recv"],
                    "total_bytes_sent": best["bytes_sent"],
                    # Link state only. No external host is contacted, so this
                    # reports local connectivity rather than internet reach.
                    "connectivity": "LINK UP",
                }
            )
        return result

    # ------------------------------------------------------------- processes

    def sample_processes(self) -> dict[str, Any]:
        """Process and thread counts. Iterating processes is comparatively
        expensive, so this belongs on a slow loop."""
        total = running = sleeping = threads = 0
        try:
            for proc in psutil.process_iter(attrs=["status", "num_threads"]):
                info = proc.info
                total += 1
                status = info.get("status") or ""
                if status == psutil.STATUS_RUNNING:
                    running += 1
                elif status == psutil.STATUS_SLEEPING:
                    sleeping += 1
                threads += int(info.get("num_threads") or 0)
        except Exception:
            return {"total": None, "running": None, "sleeping": None, "threads": None}
        return {"total": total, "running": running, "sleeping": sleeping, "threads": threads}

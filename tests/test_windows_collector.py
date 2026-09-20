"""Windows collection, percentage maths, rate maths and absent drives."""

from __future__ import annotations

import time
from collections import namedtuple

import psutil
import pytest

from backend.collectors.windows_collector import WindowsCollector, _RateTracker, percent_of


# --------------------------------------------------------------- percentages


def test_percent_of_basic():
    assert percent_of(50, 200) == 25.0
    assert percent_of(206262779904, 254679183360) == 81.0


def test_percent_of_rejects_unusable_denominator():
    assert percent_of(10, 0) is None
    assert percent_of(10, None) is None
    assert percent_of(None, 100) is None
    assert percent_of("x", 100) is None


def test_percent_of_clamps_to_full_scale():
    # A counter anomaly must not drive a donut past 100 or below zero.
    assert percent_of(150, 100) == 100.0
    assert percent_of(-5, 100) == 0.0


# ---------------------------------------------------------------- rate maths


def test_rate_tracker_first_sample_has_no_rate():
    tracker = _RateTracker()
    assert tracker.rate("k", 1000) is None


def test_rate_tracker_computes_per_second(monkeypatch):
    tracker = _RateTracker()
    clock = [100.0]
    monkeypatch.setattr(time, "monotonic", lambda: clock[0])
    assert tracker.rate("k", 1000) is None
    clock[0] = 102.0
    assert tracker.rate("k", 3000) == pytest.approx(1000.0)


def test_rate_tracker_treats_counter_reset_as_zero(monkeypatch):
    tracker = _RateTracker()
    clock = [100.0]
    monkeypatch.setattr(time, "monotonic", lambda: clock[0])
    tracker.rate("k", 5000)
    clock[0] = 101.0
    # A smaller value means the counter restarted, not a negative throughput.
    assert tracker.rate("k", 10) == 0.0


def test_rate_tracker_ignores_none():
    assert _RateTracker().rate("k", None) is None


# ------------------------------------------------------------- cpu breakdown

CpuTimes = namedtuple("CpuTimes", "user system idle interrupt dpc")


def test_cpu_breakdown_from_times():
    previous = CpuTimes(user=10.0, system=5.0, idle=85.0, interrupt=0.0, dpc=0.0)
    current = CpuTimes(user=30.0, system=15.0, idle=105.0, interrupt=0.0, dpc=0.0)
    result = WindowsCollector._times_breakdown(previous, current)
    # 20 user + 10 system + 20 idle = 50 of wall time; busy is everything else.
    assert result["user"] == 40.0
    assert result["system"] == 20.0
    assert result["idle"] == 40.0
    assert result["busy"] == 60.0


def test_cpu_breakdown_without_elapsed_time_is_none():
    same = CpuTimes(user=1.0, system=1.0, idle=1.0, interrupt=0.0, dpc=0.0)
    assert WindowsCollector._times_breakdown(same, same) is None


def test_cpu_sample_reports_none_before_an_interval_has_passed(settings):
    collector = WindowsCollector(settings)
    # The baseline was taken in the constructor a moment ago, so there is not
    # yet enough elapsed time to divide by.
    sample = collector.sample_cpu()
    assert sample["percent"] is None
    assert sample["cores_logical"] == psutil.cpu_count(logical=True)


def test_cpu_sample_produces_a_percentage_after_an_interval(settings):
    collector = WindowsCollector(settings)
    # A real pause is used rather than a patched clock: the percentages come
    # from CPU time deltas, which only accumulate as time actually passes.
    time.sleep(0.35)
    sample = collector.sample_cpu()
    assert sample["percent"] is not None
    assert 0.0 <= sample["percent"] <= 100.0
    assert len(sample["per_core"]) == psutil.cpu_count(logical=True)
    for core in sample["per_core"]:
        assert core["percent"] is None or 0.0 <= core["percent"] <= 100.0


# ------------------------------------------------------------------- memory


def test_memory_sample_is_consistent(settings):
    memory = WindowsCollector(settings).sample_memory()
    assert memory["total_bytes"] > 0
    assert 0.0 <= memory["percent"] <= 100.0
    assert memory["used_bytes"] <= memory["total_bytes"]


# ------------------------------------------------------------- filesystems


def _partition(mount: str):
    Partition = namedtuple("Partition", "device mountpoint fstype opts")
    return Partition(device=mount, mountpoint=mount, fstype="NTFS", opts="rw,fixed")


def _usage(total: int, used: int):
    Usage = namedtuple("Usage", "total used free percent")
    return Usage(total=total, used=used, free=total - used, percent=round(used / total * 100, 1))


def test_missing_external_disk_is_listed_as_not_present(make_settings, monkeypatch):
    """A configured drive that is detached stays on the dashboard as absent."""
    settings = make_settings(DISK_MOUNTS="C:\\,E:\\", DISK_AUTODISCOVER="false")
    collector = WindowsCollector(settings)

    monkeypatch.setattr(psutil, "disk_partitions", lambda all=False: [_partition("C:\\")])

    def fake_usage(mount):
        if mount == "C:\\":
            return _usage(254679183360, 206262779904)
        # Passing an errno would make Python raise a subclass; a bare OSError
        # keeps the test about the collector rather than about errno mapping.
        raise OSError("The system cannot find the path specified")

    monkeypatch.setattr(psutil, "disk_usage", fake_usage)

    entries = {fs["mount"]: fs for fs in collector.sample_filesystems()}
    assert entries["C:\\"]["present"] is True
    assert entries["C:\\"]["percent"] == 81.0

    absent = entries["E:\\"]
    assert absent["present"] is False
    assert absent["percent"] is None
    assert absent["total_bytes"] is None
    assert absent["error"] == "OSError"


def test_permission_error_on_a_mount_does_not_stop_collection(make_settings, monkeypatch):
    settings = make_settings(DISK_MOUNTS="C:\\,X:\\", DISK_AUTODISCOVER="false")
    collector = WindowsCollector(settings)
    monkeypatch.setattr(psutil, "disk_partitions", lambda all=False: [])

    def fake_usage(mount):
        if mount == "X:\\":
            raise PermissionError("access denied")
        return _usage(1000, 500)

    monkeypatch.setattr(psutil, "disk_usage", fake_usage)

    entries = {fs["mount"]: fs for fs in collector.sample_filesystems()}
    assert entries["C:\\"]["percent"] == 50.0
    assert entries["X:\\"]["present"] is False


def test_autodiscovery_finds_real_volumes(settings):
    entries = WindowsCollector(settings).sample_filesystems()
    assert entries, "at least the system volume is expected"
    present = [e for e in entries if e["present"]]
    assert present
    for entry in present:
        assert 0.0 <= entry["percent"] <= 100.0


# ---------------------------------------------------------------- disk i/o


def test_disk_io_rates_need_two_samples(settings):
    collector = WindowsCollector(settings)
    first = collector.sample_disk_io()
    assert first["read_bytes_per_sec"] is None
    second = collector.sample_disk_io()
    # Throughput is reported as a rate, distinct from capacity utilisation.
    assert second["read_bytes_per_sec"] is None or second["read_bytes_per_sec"] >= 0.0


def test_disk_io_survives_an_unavailable_counter(settings, monkeypatch):
    collector = WindowsCollector(settings)
    monkeypatch.setattr(psutil, "disk_io_counters", lambda perdisk=False: None)
    result = collector.sample_disk_io()
    assert result["read_bytes_per_sec"] is None
    assert result["per_disk"] == []


# ----------------------------------------------------------------- network


def test_network_identifies_an_interface_and_reports_totals(settings):
    collector = WindowsCollector(settings)
    collector.sample_network()
    result = collector.sample_network()
    assert result["per_interface"], "interfaces are expected on this host"
    if result["interface"] is not None:
        assert result["connectivity"] == "LINK UP"
        assert result["total_bytes_recv"] >= 0
    else:
        assert result["connectivity"] == "DOWN"


def test_network_handles_a_collection_failure(settings, monkeypatch):
    collector = WindowsCollector(settings)

    def boom(*args, **kwargs):
        raise RuntimeError("counters unavailable")

    monkeypatch.setattr(psutil, "net_io_counters", boom)
    result = collector.sample_network()
    assert result["connectivity"] == "UNKNOWN"
    assert result["per_interface"] == []


def test_system_info_reports_uptime(settings):
    info = WindowsCollector(settings).system_info()
    assert info["hostname"]
    assert info["uptime_seconds"] > 0

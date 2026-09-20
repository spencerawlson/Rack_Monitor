"""Pydantic models describing the public API payload.

Every measured field is optional. A missing hardware sensor, an unmounted
drive or a truncated Proxmox response produces None, which the frontend
renders as N/A. Nothing is defaulted to zero, because zero is a reading and
None is the absence of one.
"""

from __future__ import annotations

from pydantic import BaseModel, ConfigDict, Field


class Strict(BaseModel):
    """Base model that ignores unknown keys instead of failing on them."""

    model_config = ConfigDict(extra="ignore")


class SystemInfo(Strict):
    hostname: str | None = None
    os_name: str | None = None
    os_version: str | None = None
    platform: str | None = None
    cpu_name: str | None = None
    boot_time: float | None = None
    uptime_seconds: float | None = None


class LoadAverage(Strict):
    """Windows has no true load average; psutil emulates it after a warm-up."""

    min1: float | None = None
    min5: float | None = None
    min15: float | None = None
    emulated: bool = False


class CoreUsage(Strict):
    core: int
    percent: float | None = None


class CpuMetrics(Strict):
    percent: float | None = None
    user: float | None = None
    system: float | None = None
    idle: float | None = None
    interrupt: float | None = None
    dpc: float | None = None
    cores_physical: int | None = None
    cores_logical: int | None = None
    freq_current_mhz: float | None = None
    freq_min_mhz: float | None = None
    freq_max_mhz: float | None = None
    ctx_switches_per_sec: float | None = None
    interrupts_per_sec: float | None = None
    per_core: list[CoreUsage] = Field(default_factory=list)
    load: LoadAverage = Field(default_factory=LoadAverage)


class MemoryMetrics(Strict):
    total_bytes: int | None = None
    used_bytes: int | None = None
    available_bytes: int | None = None
    free_bytes: int | None = None
    percent: float | None = None


class SwapMetrics(Strict):
    total_bytes: int | None = None
    used_bytes: int | None = None
    free_bytes: int | None = None
    percent: float | None = None


class FilesystemEntry(Strict):
    """One mount point. present=False keeps a configured-but-absent disk visible."""

    mount: str
    device: str | None = None
    fstype: str | None = None
    options: str | None = None
    total_bytes: int | None = None
    used_bytes: int | None = None
    free_bytes: int | None = None
    percent: float | None = None
    present: bool = True
    error: str | None = None


class PerDiskIo(Strict):
    name: str
    read_bytes_per_sec: float | None = None
    write_bytes_per_sec: float | None = None


class DiskIoMetrics(Strict):
    """Disk activity, kept distinct from disk capacity utilisation."""

    read_bytes_per_sec: float | None = None
    write_bytes_per_sec: float | None = None
    read_count_per_sec: float | None = None
    write_count_per_sec: float | None = None
    per_disk: list[PerDiskIo] = Field(default_factory=list)


class PerInterfaceNet(Strict):
    name: str
    up: bool | None = None
    speed_mbps: int | None = None
    download_bytes_per_sec: float | None = None
    upload_bytes_per_sec: float | None = None
    bytes_recv: int | None = None
    bytes_sent: int | None = None


class NetworkMetrics(Strict):
    interface: str | None = None
    ip_address: str | None = None
    ip_mask_cidr: int | None = None
    link_up: bool | None = None
    speed_mbps: int | None = None
    download_bytes_per_sec: float | None = None
    upload_bytes_per_sec: float | None = None
    total_bytes_recv: int | None = None
    total_bytes_sent: int | None = None
    connectivity: str = "UNKNOWN"
    per_interface: list[PerInterfaceNet] = Field(default_factory=list)


class ProcessCounts(Strict):
    total: int | None = None
    running: int | None = None
    sleeping: int | None = None
    threads: int | None = None


class TemperatureMetrics(Strict):
    """A reading is reported only when a real sensor supplied it."""

    cpu_celsius: float | None = None
    source: str | None = None
    available: bool = False
    detail: str | None = None


class PhysicalDiskHealth(Strict):
    name: str | None = None
    media_type: str | None = None
    health: str | None = None
    operational: str | None = None
    size_bytes: int | None = None


class DiskHealthMetrics(Strict):
    available: bool = False
    detail: str | None = None
    disks: list[PhysicalDiskHealth] = Field(default_factory=list)


class PrimaryGauges(Strict):
    """The three values driving the donut charts for a section."""

    cpu_percent: float | None = None
    memory_percent: float | None = None
    disk_percent: float | None = None
    disk_mount: str | None = None


class WindowsMetrics(Strict):
    system: SystemInfo = Field(default_factory=SystemInfo)
    cpu: CpuMetrics = Field(default_factory=CpuMetrics)
    memory: MemoryMetrics = Field(default_factory=MemoryMetrics)
    swap: SwapMetrics = Field(default_factory=SwapMetrics)
    filesystems: list[FilesystemEntry] = Field(default_factory=list)
    disk_io: DiskIoMetrics = Field(default_factory=DiskIoMetrics)
    network: NetworkMetrics = Field(default_factory=NetworkMetrics)
    processes: ProcessCounts = Field(default_factory=ProcessCounts)
    temperature: TemperatureMetrics = Field(default_factory=TemperatureMetrics)
    disk_health: DiskHealthMetrics = Field(default_factory=DiskHealthMetrics)
    primary: PrimaryGauges = Field(default_factory=PrimaryGauges)


class ProxmoxGuestSummary(Strict):
    """None total means the token lacked permission to enumerate guests."""

    running: int | None = None
    total: int | None = None
    permitted: bool = True


class ProxmoxStorageEntry(Strict):
    name: str | None = None
    type: str | None = None
    total_bytes: int | None = None
    used_bytes: int | None = None
    available_bytes: int | None = None
    percent: float | None = None
    enabled: bool | None = None


class ProxmoxNodeMetrics(Strict):
    key: str
    name: str
    status: str
    api_node: str | None = None
    message: str | None = None
    last_attempt: str | None = None
    last_success: str | None = None
    stale: bool = False
    consecutive_failures: int = 0
    primary: PrimaryGauges = Field(default_factory=PrimaryGauges)
    cpu_percent: float | None = None
    cpu_count: int | None = None
    memory_total_bytes: int | None = None
    memory_used_bytes: int | None = None
    memory_percent: float | None = None
    rootfs_total_bytes: int | None = None
    rootfs_used_bytes: int | None = None
    rootfs_percent: float | None = None
    swap_percent: float | None = None
    uptime_seconds: float | None = None
    load: LoadAverage = Field(default_factory=LoadAverage)
    pve_version: str | None = None
    kernel: str | None = None
    temperature: TemperatureMetrics = Field(default_factory=TemperatureMetrics)
    storage: list[ProxmoxStorageEntry] = Field(default_factory=list)
    storage_total_bytes: int | None = None
    storage_available_bytes: int | None = None
    vms: ProxmoxGuestSummary = Field(default_factory=ProxmoxGuestSummary)
    containers: ProxmoxGuestSummary = Field(default_factory=ProxmoxGuestSummary)


class CollectorState(Strict):
    """Per-loop liveness, used to mark a section stale rather than to hide it."""

    last_attempt: str | None = None
    last_success: str | None = None
    last_error: str | None = None
    stale: bool = False
    runs: int = 0
    failures: int = 0


class ServiceState(Strict):
    status: str = "STARTING"
    started_at: str | None = None
    uptime_seconds: float | None = None
    version: str = "1.0.0"
    collectors: dict[str, CollectorState] = Field(default_factory=dict)
    config_warnings: list[str] = Field(default_factory=list)


class DashboardSnapshot(Strict):
    generated_at: str
    service: ServiceState = Field(default_factory=ServiceState)
    windows: WindowsMetrics = Field(default_factory=WindowsMetrics)
    proxmox: dict[str, ProxmoxNodeMetrics] = Field(default_factory=dict)

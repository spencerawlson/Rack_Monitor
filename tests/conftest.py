"""Shared fixtures.

Configuration is rebuilt from a clean environment for each test, so a value in
a developer's own .env cannot change what the tests assert.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from backend.config import Settings, load_settings  # noqa: E402

# Every variable the loader reads, cleared before a settings object is built.
_MANAGED_PREFIXES = (
    "APP_",
    "THRESHOLD_",
    "TEMP_",
    "INTERVAL_",
    "STREAM_",
    "STALE_",
    "DISK_",
    "PRIMARY_DISK_",
    "LHM_",
    "SENSOR_",
    "PROXMOX_",
    "DEV_",
)


@pytest.fixture
def clean_env(monkeypatch: pytest.MonkeyPatch) -> pytest.MonkeyPatch:
    import os

    for name in list(os.environ):
        if name.startswith(_MANAGED_PREFIXES):
            monkeypatch.delenv(name, raising=False)
    return monkeypatch


@pytest.fixture
def settings(clean_env: pytest.MonkeyPatch) -> Settings:
    return load_settings()


@pytest.fixture
def make_settings(clean_env: pytest.MonkeyPatch):
    """Build settings from an explicit set of environment values."""

    def _make(**env: str) -> Settings:
        for key, value in env.items():
            clean_env.setenv(key, str(value))
        return load_settings()

    return _make

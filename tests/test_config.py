"""Configuration validation: bad values are reported, never fatal."""

from __future__ import annotations

from backend.config import STATUS_CONFIG_ERROR, STATUS_UNCONFIGURED


def test_defaults_bind_to_loopback(settings):
    assert settings.app_host == "127.0.0.1"
    assert settings.app_port == 8765


def test_nodes_start_unconfigured(settings):
    assert len(settings.nodes) == 2
    for node in settings.nodes:
        assert node.configured is False
        assert node.unconfigured_reason == "Host not set"
        assert node.status_when_unreachable == STATUS_UNCONFIGURED


def test_non_numeric_port_falls_back_and_reports(make_settings):
    settings = make_settings(APP_PORT="not-a-port")
    assert settings.app_port == 8765
    assert any("APP_PORT" in message for message in settings.errors)


def test_out_of_range_port_rejected(make_settings):
    settings = make_settings(APP_PORT="70000")
    assert settings.app_port == 8765
    assert any("outside" in message for message in settings.errors)


def test_interval_below_floor_is_clamped_to_default(make_settings):
    settings = make_settings(INTERVAL_CPU_MEM="0.01")
    assert settings.interval_cpu_mem == 1.5
    assert any("INTERVAL_CPU_MEM" in message for message in settings.errors)


def test_inverted_thresholds_are_swapped(make_settings):
    settings = make_settings(THRESHOLD_WARNING="95", THRESHOLD_CRITICAL="70")
    assert settings.usage.warning == 70.0
    assert settings.usage.critical == 95.0
    assert any("swapped" in message for message in settings.errors)


def test_temperature_thresholds_are_independent_of_usage(make_settings):
    settings = make_settings(THRESHOLD_WARNING="50", TEMP_WARNING_C="80")
    assert settings.usage.warning == 50.0
    assert settings.temperature.warning == 80.0
    # The same number means different things on each scale: 60 is a warning
    # level of utilisation here, but an unremarkable temperature.
    assert settings.usage.state(60.0) == "warning"
    assert settings.temperature.state(60.0) == "normal"


def test_threshold_states(settings):
    assert settings.usage.state(None) == "unavailable"
    assert settings.usage.state(10.0) == "normal"
    assert settings.usage.state(70.0) == "warning"
    assert settings.usage.state(89.9) == "warning"
    assert settings.usage.state(90.0) == "critical"


def test_malformed_token_id_is_a_config_error(make_settings):
    settings = make_settings(
        PROXMOX_NODE_1_HOST="10.20.20.5",
        PROXMOX_NODE_1_TOKEN_ID="monitor-without-realm",
        PROXMOX_NODE_1_TOKEN_SECRET="secret-value",
    )
    node = settings.node("node1")
    assert node.configured is False
    assert node.status_when_unreachable == STATUS_CONFIG_ERROR
    assert "user@realm" in node.unconfigured_reason


def test_missing_ca_bundle_is_reported_not_silently_ignored(make_settings):
    settings = make_settings(
        PROXMOX_NODE_1_HOST="10.20.20.5",
        PROXMOX_NODE_1_TOKEN_ID="monitor@pve!plh",
        PROXMOX_NODE_1_TOKEN_SECRET="secret-value",
        PROXMOX_NODE_1_CA_CERT_PATH=r"C:\does\not\exist\ca.pem",
    )
    node = settings.node("node1")
    assert node.configured is False
    assert "CA certificate not found" in node.unconfigured_reason
    # Falling back to the public trust store silently would change what is
    # trusted, so verification stays on and the node is not polled.
    assert node.verify_tls is True


def test_tls_verification_is_on_by_default(make_settings):
    settings = make_settings(
        PROXMOX_NODE_1_HOST="10.20.20.5",
        PROXMOX_NODE_1_TOKEN_ID="monitor@pve!plh",
        PROXMOX_NODE_1_TOKEN_SECRET="secret-value",
    )
    node = settings.node("node1")
    assert node.verify_tls is True
    assert node.verify_option() is True
    assert node.configured is True


def test_disabling_tls_requires_explicit_opt_in_and_warns(make_settings):
    settings = make_settings(
        PROXMOX_NODE_1_HOST="10.20.20.5",
        PROXMOX_NODE_1_TOKEN_ID="monitor@pve!plh",
        PROXMOX_NODE_1_TOKEN_SECRET="secret-value",
        PROXMOX_NODE_1_VERIFY_TLS="false",
    )
    node = settings.node("node1")
    assert node.verify_option() is False
    assert any("TLS verification is disabled" in message for message in settings.errors)


def test_public_config_never_contains_credentials(make_settings):
    secret = "3f8b1c2d-0000-4444-8888-aaaaaaaaaaaa"
    settings = make_settings(
        PROXMOX_NODE_1_HOST="10.20.20.5",
        PROXMOX_NODE_1_TOKEN_ID="monitor@pve!plh",
        PROXMOX_NODE_1_TOKEN_SECRET=secret,
    )
    payload = repr(settings.public_dict())
    assert secret not in payload
    assert "monitor@pve!plh" not in payload


def test_node_repr_hides_secret(make_settings):
    secret = "must-not-appear"
    settings = make_settings(
        PROXMOX_NODE_1_HOST="10.20.20.5",
        PROXMOX_NODE_1_TOKEN_ID="monitor@pve!plh",
        PROXMOX_NODE_1_TOKEN_SECRET=secret,
    )
    # A node object reaching a log line must not print its token.
    assert secret not in repr(settings.node("node1"))


def test_explicit_disk_mounts_are_parsed(make_settings):
    settings = make_settings(DISK_MOUNTS=r"C:\,D:\,E:\ ")
    assert settings.disk_mounts == ("C:\\", "D:\\", "E:\\")

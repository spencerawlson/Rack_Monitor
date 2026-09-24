# PLH Rack Monitor — Rust edition

A local, real-time dashboard for the machine it runs on and any number of
Proxmox VE nodes. One self-contained executable: no Python, no runtime, no
installer toolkit. It installs per user without administrator rights and
adapts to the machine, the operating system and the screen it finds.

- **Local only.** Serves on `127.0.0.1`; no cloud service, no telemetry, and
  no outbound connection except to the Proxmox nodes you configure.
- **One file.** `plh-rack-monitor.exe` (6.6 MB; the zip is 2.7 MB) carries the web page, the
  collectors and the installer. On Windows it needs no Visual C++ runtime.
- **Adapts.** The host is detected, not configured: CPU model and core count,
  every fixed volume, the active network adapter, the OS. The page lays itself
  out for whatever window it is in, from a 1424×280 strip to a 4K monitor.
  Any number of Proxmox nodes can be listed; each gets a panel.

---

## Contents

- [Install](#install)
- [Moving from the Python edition](#moving-from-the-python-edition)
- [Using it](#using-it)
- [Configuration](#configuration)
- [Proxmox nodes](#proxmox-nodes)
- [What adapts to the machine](#what-adapts-to-the-machine)
- [Building from source](#building-from-source)
- [Security](#security)
- [Troubleshooting](#troubleshooting)
- [What has and has not been tested](#what-has-and-has-not-been-tested)

---

## Install

### Windows

1. Unzip `plh-rack-monitor-<version>-windows-x86_64.zip` anywhere.
2. Double-click **`install.cmd`**.

That copies the program to `%LOCALAPPDATA%\Programs\PLH Rack Monitor\`, writes
a default configuration to `%APPDATA%\PLH Rack Monitor\config\config.toml`,
adds a **Start Menu** shortcut, registers it under **Settings → Apps** (so it
uninstalls like any other app), and opens the dashboard.

To also start it when you sign in:

```powershell
install.cmd --autostart
```

Nothing is added to startup unless `--autostart` is given.

From a terminal, the same thing:

```powershell
.\plh-rack-monitor.exe install              # Start Menu + Settings > Apps, then open it
.\plh-rack-monitor.exe install --autostart  # ...and start at sign-in
.\plh-rack-monitor.exe install --no-launch --no-register --dir D:\Tools\PLH
```

### Linux and macOS

Build from source (see [Building from source](#building-from-source)), then:

```bash
./plh-rack-monitor install              # ~/.local/bin (Linux) or ~/Applications (macOS)
./plh-rack-monitor install --autostart  # systemd user service / LaunchAgent
```

On Linux `--autostart` installs a systemd **user** service running the server,
plus an autostart entry that opens the page when a desktop session starts.
Run `loginctl enable-linger $USER` if it should run before anyone signs in.

### Portable use

Put a `config.toml` next to the executable and it is used instead of the
per-user one; set `PLH_DATA_DIR` to keep the log and instance record there
too. Nothing needs installing.

### Uninstall

**Settings → Apps → PLH Rack Monitor → Uninstall**, or `uninstall.cmd`, or:

```powershell
plh-rack-monitor uninstall          # keeps config.toml
plh-rack-monitor uninstall --purge  # removes configuration and logs too
```

Uninstall removes exactly what install recorded creating, nothing else.

---

## Moving from the Python edition

Your existing `.env` converts in one step, token and CA included:

```powershell
cd C:\Users\<you>\Desktop\PLH_Rack_Monitor
.\rust\target\release\plh-rack-monitor.exe import-env .env           # preview, secret masked
.\rust\target\release\plh-rack-monitor.exe import-env .env --write   # save config.toml
```

`--write` copies the CA certificate into the configuration directory's
`certs\` folder and points the config at the copy, so the installed program no
longer depends on the project folder.

The `.env` is read the way python-dotenv reads it: an unquoted Windows path
such as `C:\Users\you\certs\pve-root-ca.pem` keeps its backslashes.

**Ports.** The Python edition serves your panel on **8766**, and `import-env`
carries `APP_PORT=8766` across. Stop the Python backend first
(`.\stop_monitor.ps1`), or give the Rust edition another port in
`config.toml` (`[server] port = 8765`), or both will want 8766 and the second
one will refuse to start.

You can also run straight from the `.env` without converting anything:

```powershell
plh-rack-monitor --env-file C:\...\PLH_Rack_Monitor\.env run
```

---

## Using it

```
plh-rack-monitor              start if needed, then open the dashboard (same as `run`)
plh-rack-monitor run          ...with --no-browser, or --background to detach
plh-rack-monitor serve        run the server in the foreground, no browser
plh-rack-monitor open         open the dashboard of a running server
plh-rack-monitor status       is it running; how is each collector doing
plh-rack-monitor stop         stop it
plh-rack-monitor config path|show|init|check
plh-rack-monitor import-env <.env> [--write] [--force]
plh-rack-monitor install [--autostart] | uninstall [--purge]
```

Global options: `--config FILE`, `--env-file FILE`, `--port N`, `--bind ADDR`.

Starting it twice does not start a second server; the second call opens the
running one. `stop` asks the server to shut down through a token-protected
endpoint, and only falls back to ending the process if that process is this
program. An unrelated program on the same port is never stopped.

### On the dashboard

| Key | Action |
| --- | --- |
| `F11` | fullscreen (native first, Fullscreen API as fallback) |
| `0` / `A` | all machines |
| `1` … `9` | one machine alone (1 is this host, then nodes in config order) |
| `←` `→` | previous / next page, when there are pages |
| `D` | detail view for this host: per-core bars, CPU state split, swap, every volume, per-volume I/O, drive health, per-adapter traffic |
| `Esc` | close the detail view |

Clicking a panel isolates it; clicking again returns to all. The buttons in
the header do the same, and the choice is remembered by the browser.

The header shows the live-stream state (`LIVE`, `POLLING`, `STALLED`), an
overall verdict, and a `! n` badge when the configuration has problems —
click it to read them.

---

## Configuration

`plh-rack-monitor config path` prints the file in use. The first `install`
writes a fully commented default; `config.example.toml` in this folder is the
same file. Every setting is optional.

Which file is used, first match wins:

1. `--config FILE`
2. the `PLH_CONFIG` environment variable
3. `config.toml` beside the executable (portable use)
4. the per-user file (`%APPDATA%\PLH Rack Monitor\config\config.toml`,
   `~/.config/plhrackmonitor/config.toml`, or
   `~/Library/Application Support/PLH-Rack-Monitor/config.toml`)

A mistake never stops the dashboard. An unknown key, an out-of-range number or
a missing certificate is replaced by the default and listed by
`config check` and by the `!` badge on the page.

Main sections: `[server]` (address, port), `[browser]` (which browser, app
window, fullscreen, window size), `[display]` (title, paging interval),
`[thresholds]` (usage and temperature colour limits, separately),
`[intervals]` (per metric family), `[host]` (label, volumes to always show,
primary volume, sensors), and one `[[proxmox]]` block per node.

For the 1424×280 panel, a dedicated app window at exactly that size:

```toml
[browser]
app_window = true
window_width = 1424
window_height = 280
```

---

## Proxmox nodes

Add one `[[proxmox]]` block per node; the dashboard shows as many panels as
there are blocks.

Create a read-only token. **In a cluster, do this once on any node** — users
and tokens are cluster-wide. For standalone nodes, on each:

```bash
pveum user add monitor@pve
pveum acl modify / --users monitor@pve --roles PVEAuditor
pveum user token add monitor@pve plh --privsep 0
```

```toml
[[proxmox]]
name = "PVE"
host = "192.168.1.11"
token_id = "monitor@pve!plh"
token_secret_env = "PLH_PVE_SECRET"   # or token_secret = "...", or token_secret_file = "..."
ca_cert = "certs/pve-root-ca.pem"     # the cluster CA: /etc/pve/pve-root-ca.pem

[[proxmox]]
name = "PVE02"
host = "192.168.1.12"
token_id = "monitor@pve!plh"
token_secret_env = "PLH_PVE_SECRET"
ca_cert = "certs/pve-root-ca.pem"
```

- `node` (the API node name) can be left empty. It is discovered from
  `/nodes`, and in a cluster from the `local` flag in `/cluster/status`.
- TLS verification is always on unless `verify_tls = false` is written for
  that node, which is then listed as a warning. With `ca_cert` set, only that
  CA is trusted; without it, the operating system's trust store is used.
- Figures come only from per-node endpoints, so two members of one cluster
  never double-count anything.
- A node that stops answering keeps its last figures, dimmed and marked
  `STALE` with the time they were read. Retries back off to once a minute.
- States: `ONLINE`, `OFFLINE`, `AUTH_ERROR` (401/403), `UNCONFIGURED`,
  `CONFIG_ERROR`, `CONNECTING`.

---

## What adapts to the machine

| | Windows | Linux | macOS |
| --- | --- | --- | --- |
| CPU %, per core | kernel counters (same method as psutil/Glances) | `/proc/stat` | sysinfo |
| CPU state split | user, system, idle, DPC, interrupt | user, system, idle, iowait, irq | not available |
| Interrupts / context switches per s | interrupts | both | — |
| Load average | not available on Windows (shown N/A) | yes | yes |
| Volumes, capacity, per-volume I/O | yes | yes (container and snap mounts skipped) | yes |
| Network rates, IPv4, link speed and state | yes | yes (`/sys/class/net`) | rates and IPv4 only |
| Processes / threads | Toolhelp snapshot | `/proc` | processes only |
| CPU temperature | LibreHardwareMonitor (web server or WMI); ACPI if permitted | hwmon (coretemp, k10temp …) | SMC |
| Drive health | Storage provider (`Get-PhysicalDisk`) | `smartctl` (needs root) | not implemented |
| Browser | Chrome, Edge, Chromium app window; else default | same, from `PATH`; else `xdg-open` | same; else `open` |
| Install / autostart | Start Menu, Settings > Apps, `Run` key | desktop entry, systemd user unit | LaunchAgent |

Anything unavailable is shown as **N/A** with the reason. No value is ever
estimated: no temperature sensor means `TEMP N/A`, not a plausible guess.

**Screens.** The page fills its window. It picks the column and row count that
shows the machines largest, and every size inside a panel scales from that
panel's own dimensions. One machine alone on a wide strip switches to larger
rings and a four-column layout. When more machines are configured than fit
legibly (text would fall below about 85% of its design size), the combined
view pages through them, `PAGE 1/2` in the header, every `page_seconds`.

---

## Building from source

Requires Rust 1.85 or newer.

**Windows** (MSVC toolchain; the C runtime is linked statically by
`.cargo/config.toml`):

```powershell
rustup toolchain install stable-x86_64-pc-windows-msvc
cargo +stable-x86_64-pc-windows-msvc build --release
.\packaging\package.ps1          # builds and writes dist\*.zip + .sha256
```

The GNU toolchain also works, but on machines with MSYS2 the `mingw64`
directory must come before `ucrt64` on `PATH`, or linking fails.

A Windows build compiles `app.rc` into the executable so it carries the rack
icon, which needs a resource compiler: `rc.exe` from the Windows SDK under
MSVC, or `windres` under GNU. Both ship with their toolchain's usual install.

**Linux / macOS:**

```bash
cargo build --release
./target/release/plh-rack-monitor install
```

A C compiler is needed for the TLS library (`ring`). Without Proxmox support
the build is pure Rust:

```bash
cargo build --release --no-default-features
```

Tests: `cargo test` (112 tests; Proxmox responses are served by a local mock
server, so no node is needed).

---

## Security

- Binds to `127.0.0.1`. Binding elsewhere is allowed but listed as a warning:
  there is no authentication.
- Requests must carry a loopback `Host` header, so a web page on another site
  cannot read the metrics through the visitor's browser (DNS rebinding).
- Strict `Content-Security-Policy`, `nosniff`, no framing, no caching.
- Proxmox tokens stay in the server process. `/api/config` never contains
  them (a test asserts it), the Authorization header is marked sensitive, and
  token text is scrubbed from every error message.
- The shutdown endpoint needs a random per-run token stored only in the
  per-user instance record.
- Only `GET` requests are sent to Proxmox; `PVEAuditor` is read-only.

---

## Troubleshooting

**"Port … is in use by another program"** — something else holds the port.
`Get-NetTCPConnection -LocalPort 8765 -State Listen` shows what; use
`--port` or `[server] port`. If it is an older PLH instance started from an
**elevated** shell, stop it from an elevated shell.

**`WRONG SERVER ON THIS PORT` banner** — the page is being served by
something other than this program (for example the Python edition, which
serves different routes).

**A node shows `OFFLINE` with an `invalid peer certificate` message** — the
node's certificate is not signed by the configured CA. Copy
`/etc/pve/pve-root-ca.pem` from the node and point `ca_cert` at it.

**`AUTH_ERROR`** — token id must be `user@realm!tokenname`; check
`pveum user token list monitor@pve` and `pveum acl list`.

**`TEMP N/A` on Windows** — expected unless LibreHardwareMonitor is running
with its web server (Options → Remote Web Server) or WMI provider enabled.

**Where are the logs?** `%LOCALAPPDATA%\PLH Rack Monitor\data\plh-rack-monitor.log`
(or `$PLH_DATA_DIR`). `plh-rack-monitor status` summarises collector health.

---

## What has and has not been tested

Verified on the development machine (Windows 11, Intel N100):

- All 102 automated tests pass: configuration validation, the python-dotenv
  compatible `.env` reader, CPU/rate/percentage maths, the `/proc` and
  `smartctl` parsers, every Proxmox failure mode against a mock server, the
  HTTP API including the Host guard and shutdown token, registry writes (on a
  scratch key) and shortcut creation.
- Live against this PC and the real two-node cluster (PVE 9.2.20) over
  verified TLS with the cluster CA: both nodes `ONLINE`, figures correct.
- The page in headless Chromium at 1424×280, 1280×720 and 1920×1080, with 3
  and 6 panels: no scrollbars, no clipped content, paging on the small panel.
- Install into a scratch directory, the installed copy running, and uninstall
  removing it (the real Start Menu and Settings > Apps entries were exercised
  through their functions, not by installing on this machine).

Not verified:

- **Linux and macOS have not been run.** Both type-check (`cargo check` for
  `x86_64-unknown-linux-gnu` and `x86_64-apple-darwin`, without the Proxmox
  TLS feature, since that needs each platform's C toolchain), and the Linux
  `/proc` and `smartctl` parsers are unit-tested, but no binary has been
  executed on either system.
- The physical GeekPi panel (only its resolution was emulated).
- A double-clicked `install.cmd` in an interactive console window.

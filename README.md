# PLH Rack Monitor

A local, real-time monitoring dashboard for one Windows 11 host and two Proxmox VE
nodes, built for a GeekPi 6.01-inch panel at **1424 x 280**.

It runs entirely on the machine it monitors. No cloud service, no internet
connection, no telemetry. The backend binds to `127.0.0.1` by default.

---

## Contents

- [What it shows](#what-it-shows)
- [Architecture](#architecture)
- [Requirements](#requirements)
- [Setup](#setup)
- [Running it](#running-it)
- [Dashboard controls](#dashboard-controls)
- [Adding the Proxmox nodes](#adding-the-proxmox-nodes)
- [TLS certificates](#tls-certificates)
- [CPU temperature](#cpu-temperature)
- [Configuration reference](#configuration-reference)
- [Starting at sign-in](#starting-at-sign-in)
- [Testing](#testing)
- [Troubleshooting](#troubleshooting)
- [Security notes](#security-notes)
- [Known limitations](#known-limitations)

---

## What it shows

Three sections: **WINDOWS HOST | PROXMOX 01 | PROXMOX 02**, each with donut charts
for CPU, RAM and DISK, plus a line of supporting figures.

**Windows host** (collected with psutil, matching what Glances reports for the
same host):

| Family | Values |
| --- | --- |
| CPU | total %, user / system / idle / DPC / interrupt split, per-core %, physical and logical core counts, frequency, context switches and interrupts per second, emulated load average |
| Memory | total, used, available, free, % used, plus swap |
| Storage | per-volume capacity, used, free and % for every fixed volume; read/write throughput overall and per physical drive; drive health from the Windows Storage provider |
| Network | active interface, IPv4 address and prefix, link state and speed, download and upload rates, lifetime totals, and per-interface rates |
| System | hostname, OS, CPU model, uptime, process and thread counts |
| Temperature | CPU temperature when a supported sensor source exists, otherwise `N/A` |

**Each Proxmox node**: CPU %, memory %, root filesystem %, swap %, uptime, load
average, PVE version, kernel, per-storage capacity, and VM/LXC counts where the
token is permitted to see them.

Capacity utilisation and disk I/O activity are shown as separate figures and are
never mixed.

---

## Architecture

```
PLH_Rack_Monitor/
├── backend/
│   ├── main.py                      FastAPI app: REST endpoints + SSE stream
│   ├── config.py                    env loading, validation, thresholds
│   ├── collectors/
│   │   ├── windows_collector.py     psutil sampling, rate maths
│   │   ├── proxmox_collector.py     Proxmox REST client, backoff, staleness
│   │   ├── temperature_collector.py CPU temperature probing (optional sources)
│   │   └── health_collector.py      physical drive health
│   ├── services/
│   │   └── metrics_service.py       independent polling loops, snapshot, SSE fan-out
│   └── models/
│       └── metrics.py               Pydantic schema for the API payload
├── frontend/
│   ├── index.html                   fixed 1424 x 280 stage
│   ├── styles.css                   ultrawide layout, threshold colours
│   └── app.js                       SVG donuts, SSE client, view modes
├── tests/
├── start_monitor.ps1 / stop_monitor.ps1
├── .env.example
└── requirements.txt
```

**Collection.** Each metric family runs in its own loop at its own interval. A loop
is strictly sequential, so requests never overlap and derived rates stay correct.
Blocking work (psutil, PowerShell) runs in worker threads, so a slow or missing
source never blocks the event loop. A loop that raises is recorded and retried on
its next tick; it never terminates.

**Delivery.** Updates reach the browser over **Server-Sent Events** at
`/api/stream`. SSE needs no extra dependency, reconnects by itself, and one-way
delivery is all a dashboard needs. If the stream cannot be established the client
falls back to polling `/api/metrics` and keeps retrying the stream.

**Endpoints**

| Endpoint | Purpose |
| --- | --- |
| `GET /` | the dashboard |
| `GET /api/health` | liveness and per-collector state; used by the start script |
| `GET /api/metrics` | current snapshot (polling fallback) |
| `GET /api/config` | thresholds, intervals, node labels — never credentials |
| `GET /api/stream` | SSE snapshot stream |

---

## Requirements

- Windows 11
- **Python 3.14.7** (`py -3.14 --version`). Install from python.org and tick
  *Add python.exe to PATH*, or use the Microsoft Store build.
- Chrome or Edge for the dashboard.

Dependencies (`requirements.txt`): fastapi, uvicorn, psutil, httpx, python-dotenv,
pydantic, pytest, pytest-asyncio.

---

## Setup

```powershell
cd "$env:USERPROFILE\Desktop\PLH_Rack_Monitor"

py -3.14 -m venv .venv
.\.venv\Scripts\python.exe -m pip install --upgrade pip
.\.venv\Scripts\python.exe -m pip install -r requirements.txt

Copy-Item .env.example .env
```

The app runs with no `.env` at all; the file is only needed to change defaults or
to add the Proxmox nodes.

---

## Running it

**Start** (starts the backend if it is not already running, waits for the health
endpoint, then opens Chrome):

```powershell
.\start_monitor.ps1
```

Useful switches:

```powershell
.\start_monitor.ps1 -App              # Chrome window with no address bar or toolbar
.\start_monitor.ps1 -App -Fullscreen  # and start fullscreen
.\start_monitor.ps1 -NoBrowser        # backend only
.\start_monitor.ps1 -Port 8790        # a different port
```

**Stop:**

```powershell
.\stop_monitor.ps1
```

**Run in the foreground** (useful when something is wrong — errors go to the
console):

```powershell
.\.venv\Scripts\python.exe -m uvicorn backend.main:app --host 127.0.0.1 --port 8765
```

Then open <http://127.0.0.1:8765/>.

Running `start_monitor.ps1` twice does not start a second backend: it checks the
health endpoint first, and refuses to continue if the port is held by something
that is not this application.

---

## Dashboard controls

| Key | Action |
| --- | --- |
| `F11` | fullscreen (native browser behaviour is used when it works; the Fullscreen API is the fallback) |
| `0` or `A` | all three machines side by side |
| `1` | Windows host on its own, full width |
| `2` | Proxmox 01 on its own |
| `3` | Proxmox 02 on its own |
| `D` | Windows detail view (per-core bars, swap, all volumes, per-NIC rates, drive health) |
| `Esc` | close the detail view |

Clicking a panel isolates it; clicking again returns to all three. The buttons in
the top right do the same. The chosen view is remembered in the browser.

The stage is exactly 1424 x 280. In any other window size the whole stage is
scaled, so the layout never reflows and no scrollbars appear.

---

## Adding the Proxmox nodes

Nothing in the source needs to change.

**If the nodes form a cluster, run this once, on any node.** Users, tokens and
permissions live in the cluster filesystem (`/etc/pve`), so they apply to every
member; running it on the second node fails with "already exists". **For two
standalone nodes, run it on each.**

```bash
# 1. A user and a read-only role assignment
pveum user add monitor@pve --comment "PLH Rack Monitor (read-only)"
pveum acl modify / --users monitor@pve --roles PVEAuditor

# 2. A token for this dashboard (privsep 0 makes the token inherit the
#    user's read-only permissions)
pveum user token add monitor@pve plh --privsep 0
```

The command prints the token secret **once**. Copy it into `.env` — in a cluster
the same token id and secret go on both nodes:

```env
PROXMOX_NODE_1_NAME=PVE
PROXMOX_NODE_1_HOST=192.168.1.11
PROXMOX_NODE_1_API_NODE=pve
PROXMOX_NODE_1_TOKEN_ID=monitor@pve!plh
PROXMOX_NODE_1_TOKEN_SECRET=xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx

PROXMOX_NODE_2_NAME=PVE02
PROXMOX_NODE_2_HOST=192.168.1.12
PROXMOX_NODE_2_API_NODE=pve02
PROXMOX_NODE_2_TOKEN_ID=monitor@pve!plh
PROXMOX_NODE_2_TOKEN_SECRET=xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx

# One CA covers every member of a cluster (see TLS certificates)
PROXMOX_CA_CERT_PATH=C:\Users\<you>\Desktop\PLH_Rack_Monitor\certs\pve-root-ca.pem
```

Then restart the backend (`.\stop_monitor.ps1`, `.\start_monitor.ps1`).

- `API_NODE` is the node name inside Proxmox (`pvecm nodes`). Setting it is the
  surest option. If it is left empty or does not match, the name is discovered:
  a standalone node is the only entry in `/nodes`; in a cluster `/nodes` lists
  every member whichever host answers, so the member that answered is taken from
  the `local` flag in `/cluster/status`. A discovery that fails (node still
  booting) is retried on the next poll rather than remembered.
- Each node is polled independently, through its own host and its own
  `/nodes/{node}/...` endpoints, so one node being down never hides the other.
  No figure is read from a cluster-wide endpoint, so two nodes cannot
  double-count shared resources; `/cluster/status` is read only to identify a node.
- `PVEAuditor` is read-only. The dashboard never writes to Proxmox.

**Node states on the dashboard**

| State | Meaning |
| --- | --- |
| `UNCONFIGURED` | no host or token yet — placeholder card, no figures |
| `CONFIG_ERROR` | the entry is malformed (bad token id, missing CA file) |
| `ONLINE` | polled successfully |
| `OFFLINE` | unreachable; the last good figures stay on screen, dimmed, with the time of that success |
| `AUTH_ERROR` | the token was rejected (401) or lacks permission (403) |

An offline node never blocks the Windows section: the loops are independent, each
request has a timeout, and repeated failures back off geometrically to a 60 second
ceiling.

---

## TLS certificates

TLS verification is **on by default** and is not disabled anywhere in the code.

Proxmox ships a self-signed certificate, which Windows will not trust as-is. Pick one:

1. **Point at the node's CA** (recommended). Copy
   `/etc/pve/pve-root-ca.pem` from the node into `certs\` (git-ignored) and set
   `PROXMOX_NODE_1_CA_CERT_PATH` to it. **In a cluster every member's
   certificate is signed by that same CA**, so one file set as
   `PROXMOX_CA_CERT_PATH` covers both nodes:

   ```powershell
   scp root@192.168.1.11:/etc/pve/pve-root-ca.pem .\certs\pve-root-ca.pem
   ```

   The node certificates carry the node's IP address as a subject alternative
   name, so connecting by IP verifies correctly. This passes Python 3.14's
   strict X.509 checks as issued; nothing is relaxed.
2. **Install a certificate from a CA you run or from Let's Encrypt** on the node,
   and leave the CA path empty if the issuer is already trusted by Windows.
3. **Last resort:** `PROXMOX_NODE_1_VERIFY_TLS=false`. This must be set
   deliberately, it applies to that node only, and the dashboard records a
   configuration warning visible in `/api/config`.

If the CA file path is set but missing, the node is reported as `CONFIG_ERROR`
rather than quietly falling back to the public trust store.

---

## CPU temperature

**On the machine this was built for, no CPU temperature source exists, so the
dashboard shows `TEMP N/A`.** This was verified rather than assumed:

- `psutil` 7.2.2 has no `sensors_temperatures` on Windows.
- Glances' own `/api/4/sensors` returns `[]` on this host.
- The `root/LibreHardwareMonitor` and `root/OpenHardwareMonitor` WMI namespaces
  are not present.
- `MSAcpi_ThermalZoneTemperature` returns *Access denied*.

No value is estimated or carried over from a stale probe. To get a real reading,
install [LibreHardwareMonitor](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor)
and enable **either**:

- *Options → Remote Web Server → Run*, which the app reads from
  `LHM_HTTP_URL` (default `http://127.0.0.1:8085/data.json`), **or**
- *Options → WMI Provider*, which the app reads through PowerShell.

The reading appears at the next temperature interval; nothing else needs changing.
LibreHardwareMonitor generally needs to run as administrator to read sensors.

Temperature thresholds (`TEMP_WARNING_C`, `TEMP_CRITICAL_C`) are configured
separately from the utilisation thresholds, because degrees and percent are not
comparable scales.

---

## Configuration reference

Every setting lives in `.env` (see `.env.example` for the annotated list).
Invalid values never stop the app: the default is used and a warning is recorded
in `/api/config` under `config_warnings`.

| Variable | Default | Notes |
| --- | --- | --- |
| `APP_HOST` / `APP_PORT` | `127.0.0.1` / `8765` | loopback only unless deliberately changed |
| `THRESHOLD_WARNING` / `THRESHOLD_CRITICAL` | `70` / `90` | donut colours; inverted values are swapped and reported |
| `TEMP_WARNING_C` / `TEMP_CRITICAL_C` | `75` / `90` | temperature only |
| `INTERVAL_CPU_MEM` / `INTERVAL_NET` | `1.5` / `1.5` | seconds |
| `INTERVAL_DISK` / `INTERVAL_PROCESSES` | `5` / `10` | |
| `INTERVAL_TEMP` / `INTERVAL_DISK_HEALTH` | `5` / `60` | sensors are not queried faster than needed |
| `INTERVAL_PROXMOX` | `4` | per node, concurrent |
| `STREAM_PUSH_INTERVAL` | `1.0` | SSE push cadence |
| `DISK_MOUNTS` | empty | e.g. `C:\,D:\,E:\`; listed drives stay visible as `NOT PRESENT` when detached |
| `DISK_AUTODISCOVER` | `true` | also report every fixed volume found |
| `PRIMARY_DISK_MOUNT` | `C:\` | the volume behind the DISK donut |
| `TEMP_ENABLED` / `LHM_WMI_ENABLED` | `true` / `true` | |
| `SENSOR_BACKOFF_SECONDS` | `60` | wait after a failed sensor probe |
| `DISK_HEALTH_ENABLED` | `true` | `Get-PhysicalDisk` |
| `DEV_FAKE_PROXMOX` | `false` | synthetic node data labelled `DEV_MOCK`; never for the wall panel |

---

## Starting at sign-in

Nothing has been changed in your Windows startup settings. To set it up yourself:

1. Open **Task Scheduler** → **Create Task**.
2. **General:** name `PLH Rack Monitor`, *Run only when user is logged on*.
3. **Triggers:** New → *At log on* → your account. Add a 15 second delay so the
   network is up.
4. **Actions:** New → *Start a program*
   - Program: `powershell.exe`
   - Arguments:
     `-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File "C:\Users\<you>\Desktop\PLH_Rack_Monitor\start_monitor.ps1" -App`
   - Start in: `C:\Users\<you>\Desktop\PLH_Rack_Monitor`
   Leave *Run with highest privileges* unticked: the dashboard needs no
   elevation, and an elevated backend cannot be stopped from a normal PowerShell.
5. **Conditions:** untick *Start the task only if the computer is on AC power*.
6. **Settings:** untick *Stop the task if it runs longer than...*.

Verify with **Run** in Task Scheduler before relying on it.

---

## Testing

```powershell
.\.venv\Scripts\python.exe -m pytest          # whole suite
.\.venv\Scripts\python.exe -m pytest -v       # per-test names
.\.venv\Scripts\python.exe -m pytest tests/test_proxmox_collector.py
```

Covered: Windows metric collection, percentage and rate maths, absent hardware
sensors, missing external disks, invalid configuration, Proxmox authentication
failures (401 and 403), node disconnection with stale-value retention, backoff,
malformed and truncated API responses, node-name discovery, credential redaction,
and the API schema.

Proxmox responses are served from recorded payloads through an httpx mock
transport, so no node is needed to run the suite.

The frontend was measured in headless Chromium at exactly 1424 x 280: the stage
renders at 1424 x 280 with scale 1, there is no horizontal or vertical scroll, no
element overflows its box in any of the four view modes, and the browser console
is clean.

---

## Troubleshooting

**`Port 8765 is in use by PID ...`** — something else holds the port. Find it with
`Get-NetTCPConnection -LocalPort 8765 -State Listen`, then stop it or use
`-Port 8790`.

**The dashboard shows `POLLING` or `RECONNECTING`** — the SSE stream dropped and
the client is using the REST fallback; data is still current, just at 2 second
resolution. `STALLED` means no frame has arrived for 8 seconds; check the backend
console.

**`TEMP N/A`** — expected unless LibreHardwareMonitor is installed and its web
server or WMI provider is enabled. See [CPU temperature](#cpu-temperature).

**A node says `AUTH_ERROR`** — the token id must be `user@realm!tokenname` and the
secret must match. Check the role assignment with
`pveum acl list` and the token with `pveum user token list monitor@pve`.

**A node says `OFFLINE` with a TLS message** — the certificate is not trusted. See
[TLS certificates](#tls-certificates).

**A node says `OFFLINE` but is reachable in a browser** — confirm the API node name
with `pvecm nodes`, and that port 8006 is reachable from the Windows host
(`Test-NetConnection 192.168.1.11 -Port 8006`).

**A node says `OFFLINE — Timed out after 4s` while it is busy** — Proxmox answers
its API slowly when its disk is saturated (a large copy, a backup, a VM import);
the storage query in particular waits on the disk. The card recovers by itself
when the load ends. Storage and guest figures that time out keep their last
good value meanwhile. For a node that is often this busy, raise
`PROXMOX_NODE_n_TIMEOUT_SECONDS`.

**`stop_monitor.ps1` says `Access is denied`** — that backend was started from an
elevated (administrator) PowerShell, and a normal PowerShell cannot stop it.
Run `.\stop_monitor.ps1` once from an administrator PowerShell, then start it
normally. The dashboard needs no elevation.

**CPU always reads 0%** — this was a real bug in an earlier build caused by
psutil's per-thread comparison state; CPU is now derived from `cpu_times` deltas
held by the collector. If you see it again, compare against Glances or Task
Manager and check `/api/health` for collector errors.

**Blank page or stale layout** — the dashboard is served with `no-store`, but a
pinned Chrome window may still hold an old script; reload with `Ctrl+F5`.

---

## Security notes

- Binds to `127.0.0.1`. The API has no authentication, so it must not be exposed
  to the LAN or the internet as-is. For remote access, put it behind an
  authenticated reverse proxy or a VPN.
- Proxmox tokens are read from the environment, held only in the backend process,
  and never sent to the browser. `/api/config` is asserted credential-free by a test.
- Token text is scrubbed from any error message that could reach a log or the UI.
- TLS verification is on by default; disabling it is per-node, explicit, and
  recorded as a warning.
- `PVEAuditor` is read-only: the dashboard issues only GET requests.
- `.env`, `*.pem`, `*.key` and `certs/` are git-ignored.
- The application makes no outbound connection other than to the Proxmox nodes you
  configure. Network connectivity is judged from local link state, not by pinging
  an external host.

---

## Known limitations

- **CPU temperature is unavailable on this host** until LibreHardwareMonitor is
  installed; the dashboard shows `N/A` rather than a guess.
- **Drive health is not a full SMART dump.** It is the Windows Storage provider's
  `HealthStatus` and `OperationalStatus` per physical disk.
- **Load average on Windows is emulated** by psutil and reads 0.00 until its
  sampling thread has been running for a few minutes.
- **Process states**: Windows reports nearly every process as `running`, so the
  running/sleeping split is not meaningful. Glances behaves the same way.
- **Proxmox exposes no temperature** through its documented API, so node
  temperature is always `N/A`.
- **Proxmox integration was verified against real hardware on 2026-09-21**: a
  two-node Proxmox VE 9.2 cluster (`pve`, `pve02`), polled by IP with full TLS
  verification against the cluster CA and a `PVEAuditor` token. The tests use
  recorded payloads, so the suite still runs with no node present.
- The 1424 x 280 layout was verified in headless Chromium at that exact viewport,
  **not** on the physical GeekPi panel.

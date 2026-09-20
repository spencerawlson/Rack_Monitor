/* PLH Rack Monitor - dashboard client.
 *
 * Elements are created once and updated in place afterwards. Nothing is
 * rebuilt with innerHTML on a tick, because replacing a node restarts its CSS
 * transition and the charts would jump instead of glide.
 *
 * Live data arrives over Server-Sent Events. If the stream cannot be
 * established the client falls back to polling, and keeps trying the stream.
 */

(function () {
  "use strict";

  var TARGET_W = 1424;
  var TARGET_H = 280;
  var SVG_NS = "http://www.w3.org/2000/svg";
  var RADIUS = 40;
  var CIRCUMFERENCE = 2 * Math.PI * RADIUS;

  var config = {
    thresholds: {
      usage: { warning: 70, critical: 90 },
      temperature: { warning: 75, critical: 90 }
    },
    nodes: []
  };

  var sections = {};
  var detailOpen = false;
  var lastSnapshot = null;
  var pollTimer = null;
  var eventSource = null;
  var lastFrameAt = 0;
  // Nothing may claim a healthy system before a snapshot has ever arrived.
  var everLoaded = false;
  var apiFailures = 0;

  /* ----------------------------------------------------------- formatting */

  function isNum(v) {
    return typeof v === "number" && isFinite(v);
  }

  function fmtPct(v) {
    return isNum(v) ? Math.round(v) + "%" : "N/A";
  }

  function fmtBytes(v, digits) {
    if (!isNum(v)) return "N/A";
    var units = ["B", "K", "M", "G", "T", "P"];
    var n = v;
    var i = 0;
    while (Math.abs(n) >= 1024 && i < units.length - 1) {
      n /= 1024;
      i += 1;
    }
    var d = digits === undefined ? (Math.abs(n) >= 100 || i === 0 ? 0 : 1) : digits;
    return n.toFixed(d) + units[i];
  }

  function fmtRate(v) {
    return isNum(v) ? fmtBytes(v) + "/s" : "N/A";
  }

  function fmtUptime(seconds) {
    if (!isNum(seconds)) return "N/A";
    var s = Math.max(0, Math.floor(seconds));
    var d = Math.floor(s / 86400);
    var h = Math.floor((s % 86400) / 3600);
    var m = Math.floor((s % 3600) / 60);
    if (d > 0) return d + "d " + h + "h";
    if (h > 0) return h + "h " + m + "m";
    return m + "m " + (s % 60) + "s";
  }

  function fmtClock(iso) {
    if (!iso) return "N/A";
    var d = new Date(iso);
    if (isNaN(d.getTime())) return "N/A";
    return d.toLocaleTimeString([], { hour12: false });
  }

  function fmtAgo(iso) {
    if (!iso) return "never";
    var d = new Date(iso);
    if (isNaN(d.getTime())) return "unknown";
    var secs = Math.max(0, (Date.now() - d.getTime()) / 1000);
    if (secs < 60) return Math.round(secs) + "s ago";
    if (secs < 3600) return Math.round(secs / 60) + "m ago";
    return Math.round(secs / 3600) + "h ago";
  }

  function usageState(v) {
    if (!isNum(v)) return "unavailable";
    if (v >= config.thresholds.usage.critical) return "critical";
    if (v >= config.thresholds.usage.warning) return "warning";
    return "normal";
  }

  function tempState(v) {
    if (!isNum(v)) return "unavailable";
    if (v >= config.thresholds.temperature.critical) return "critical";
    if (v >= config.thresholds.temperature.warning) return "warning";
    return "normal";
  }

  /* --------------------------------------------------------------- donuts */

  function makeDonut(parent, label) {
    var svg = document.createElementNS(SVG_NS, "svg");
    svg.setAttribute("viewBox", "0 0 100 100");
    svg.setAttribute("class", "donut unavailable");

    var track = document.createElementNS(SVG_NS, "circle");
    track.setAttribute("class", "track");
    track.setAttribute("cx", "50");
    track.setAttribute("cy", "50");
    track.setAttribute("r", String(RADIUS));

    var arc = document.createElementNS(SVG_NS, "circle");
    arc.setAttribute("class", "arc");
    arc.setAttribute("cx", "50");
    arc.setAttribute("cy", "50");
    arc.setAttribute("r", String(RADIUS));
    arc.setAttribute("stroke-dasharray", String(CIRCUMFERENCE));
    arc.setAttribute("stroke-dashoffset", String(CIRCUMFERENCE));

    var value = document.createElementNS(SVG_NS, "text");
    value.setAttribute("class", "value");
    value.setAttribute("x", "50");
    value.setAttribute("y", "52");
    value.textContent = "N/A";

    var caption = document.createElementNS(SVG_NS, "text");
    caption.setAttribute("class", "label");
    caption.setAttribute("x", "50");
    caption.setAttribute("y", "70");
    caption.textContent = label;

    svg.appendChild(track);
    svg.appendChild(arc);
    svg.appendChild(value);
    svg.appendChild(caption);
    parent.appendChild(svg);

    var shown = null;
    var animation = null;

    function paint(n) {
      value.textContent = Math.round(n) + "%";
    }

    function tweenTo(next) {
      // The ring is animated by CSS; only the number needs stepping so the
      // digits do not snap while the arc is still travelling.
      if (animation) cancelAnimationFrame(animation);
      var from = shown === null ? next : shown;
      var start = performance.now();
      var duration = 600;
      function step(now) {
        var t = Math.min(1, (now - start) / duration);
        var eased = 1 - Math.pow(1 - t, 3);
        paint(from + (next - from) * eased);
        if (t < 1) {
          animation = requestAnimationFrame(step);
        } else {
          animation = null;
        }
      }
      animation = requestAnimationFrame(step);
    }

    return {
      set: function (percent, stale) {
        var state = usageState(percent);
        svg.setAttribute("class", "donut " + state + (stale ? " stale" : ""));
        if (!isNum(percent)) {
          if (animation) cancelAnimationFrame(animation);
          animation = null;
          shown = null;
          value.textContent = "N/A";
          arc.setAttribute("stroke-dashoffset", String(CIRCUMFERENCE));
          return;
        }
        var clamped = Math.max(0, Math.min(100, percent));
        arc.setAttribute(
          "stroke-dashoffset",
          String(CIRCUMFERENCE * (1 - clamped / 100))
        );
        if (shown === null) {
          paint(clamped);
        } else if (Math.abs(clamped - shown) >= 0.5) {
          tweenTo(clamped);
        }
        shown = clamped;
      }
    };
  }

  /* ---------------------------------------------------------- stat blocks */

  function StatBlock(container) {
    this.container = container;
    this.rows = {};
  }

  StatBlock.prototype.set = function (key, label, value, cls, wide) {
    var row = this.rows[key];
    if (!row) {
      var el = document.createElement("div");
      el.className = "stat" + (wide ? " wide" : "");
      var k = document.createElement("span");
      k.className = "k";
      k.textContent = label;
      var v = document.createElement("span");
      v.className = "v";
      el.appendChild(k);
      el.appendChild(v);
      this.container.appendChild(el);
      row = this.rows[key] = { el: el, k: k, v: v };
    }
    if (row.k.textContent !== label) row.k.textContent = label;
    if (row.v.textContent !== value) row.v.textContent = value;
    var want = "v" + (cls ? " " + cls : "");
    if (row.v.className !== want) row.v.className = want;
    row.el.hidden = false;
  };

  StatBlock.prototype.note = function (key, text, cls) {
    var row = this.rows[key];
    if (!row) {
      var el = document.createElement("div");
      el.className = "note";
      this.container.appendChild(el);
      row = this.rows[key] = { el: el, v: el };
    }
    if (row.el.textContent !== text) row.el.textContent = text;
    row.el.className = "note" + (cls ? " " + cls : "");
    row.el.hidden = false;
  };

  StatBlock.prototype.hide = function (key) {
    var row = this.rows[key];
    if (row) row.el.hidden = true;
  };

  /* --------------------------------------------------------------- layout */

  function buildSection(key) {
    var donutRow = document.getElementById("donuts-" + key);
    var section = {
      panel: document.getElementById("panel-" + key),
      title: document.getElementById("title-" + key),
      pill: document.getElementById("pill-" + key),
      stats: new StatBlock(document.getElementById("stats-" + key)),
      donuts: {
        cpu: makeDonut(donutRow, "CPU"),
        memory: makeDonut(donutRow, "RAM"),
        disk: makeDonut(donutRow, "DISK")
      }
    };
    sections[key] = section;
    return section;
  }

  function fitStage() {
    var stage = document.getElementById("stage");
    var scale = Math.min(window.innerWidth / TARGET_W, window.innerHeight / TARGET_H);
    // At exactly 1424x280 the scale is 1 and the stage is pixel-accurate.
    stage.style.transform = scale === 1 ? "none" : "scale(" + scale + ")";
  }

  /* -------------------------------------------------------------- rendering */

  function renderWindows(win, service) {
    var s = sections.windows;
    var sys = win.system || {};
    var cpu = win.cpu || {};
    var mem = win.memory || {};
    var net = win.network || {};
    var temp = win.temperature || {};
    var health = win.disk_health || {};
    var primary = win.primary || {};
    var io = win.disk_io || {};

    s.title.textContent = "WINDOWS HOST" + (sys.hostname ? " · " + sys.hostname : "");

    var degraded = service && service.status === "DEGRADED";
    s.pill.textContent = degraded ? "DEGRADED" : "ONLINE";
    s.pill.className = "pill " + (degraded ? "warning" : "online");
    s.panel.className = "panel";

    s.donuts.cpu.set(primary.cpu_percent, false);
    s.donuts.memory.set(primary.memory_percent, false);
    s.donuts.disk.set(primary.disk_percent, false);

    var tState = tempState(temp.cpu_celsius);
    s.stats.set(
      "temp",
      "TEMP",
      isNum(temp.cpu_celsius) ? temp.cpu_celsius.toFixed(1) + "°C" : "N/A",
      tState === "unavailable" ? "dim" : tState
    );
    s.stats.set("uptime", "UP", fmtUptime(sys.uptime_seconds));
    s.stats.set(
      "net",
      "NET",
      "↓" + fmtRate(net.download_bytes_per_sec) + " ↑" + fmtRate(net.upload_bytes_per_sec)
    );
    s.stats.set(
      "io",
      "I/O",
      "R " + fmtRate(io.read_bytes_per_sec) + " W " + fmtRate(io.write_bytes_per_sec)
    );
    s.stats.set(
      "disk",
      "VOL",
      (primary.disk_mount || "?") + " " + fmtPct(primary.disk_percent)
    );
    s.stats.set(
      "ram",
      "RAM",
      isNum(mem.used_bytes) ? fmtBytes(mem.used_bytes) + "/" + fmtBytes(mem.total_bytes) : "N/A"
    );
    s.stats.set(
      "cores",
      "CPU",
      (isNum(cpu.cores_physical) ? cpu.cores_physical + "C" : "?") +
        "/" +
        (isNum(cpu.cores_logical) ? cpu.cores_logical + "T" : "?") +
        (isNum(cpu.freq_current_mhz) ? " " + Math.round(cpu.freq_current_mhz) + "MHz" : "")
    );

    var disks = health.disks || [];
    var unhealthy = disks.filter(function (d) {
      return d.health && d.health.toLowerCase() !== "healthy";
    });
    s.stats.set(
      "smart",
      "DISKS",
      health.available
        ? disks.length + " " + (unhealthy.length ? unhealthy.length + " DEGRADED" : "HEALTHY")
        : "N/A",
      health.available ? (unhealthy.length ? "critical" : "good") : "dim"
    );

    if (!temp.available && temp.detail) {
      s.stats.note("tempnote", "TEMP: " + temp.detail, null);
    } else {
      s.stats.hide("tempnote");
    }
  }

  function renderNode(key, node) {
    var s = sections[key];
    if (!node) return;

    s.title.textContent = node.name || key.toUpperCase();

    var status = node.status || "UNKNOWN";
    var stale = !!node.stale;
    var primary = node.primary || {};

    s.panel.className =
      "panel" +
      (status === "UNCONFIGURED" || status === "CONFIG_ERROR" ? " unconfigured" : "") +
      (status === "OFFLINE" || status === "AUTH_ERROR" ? " offline" : "");

    s.pill.textContent = stale ? status + " · STALE" : status;
    s.pill.className =
      "pill " +
      (status === "ONLINE"
        ? "online"
        : status === "UNCONFIGURED"
        ? "unconfigured"
        : status === "DEV_MOCK"
        ? "warning"
        : "offline");

    // An unconfigured node shows no figures at all: an empty ring, never a
    // plausible looking percentage.
    s.donuts.cpu.set(primary.cpu_percent, stale);
    s.donuts.memory.set(primary.memory_percent, stale);
    s.donuts.disk.set(primary.disk_percent, stale);

    if (status === "UNCONFIGURED" || status === "CONFIG_ERROR") {
      s.stats.set("state", "STATUS", "NOT CONFIGURED", "dim");
      s.stats.set("temp", "TEMP", "N/A", "dim");
      s.stats.hide("uptime");
      s.stats.hide("guests");
      s.stats.hide("store");
      s.stats.hide("ram");
      s.stats.note("hint", node.message ? node.message + " — see .env" : "Add host and API token in .env", null);
      return;
    }

    s.stats.set("temp", "TEMP", "N/A", "dim");
    s.stats.set("uptime", "UP", fmtUptime(node.uptime_seconds));
    s.stats.set(
      "ram",
      "RAM",
      isNum(node.memory_used_bytes)
        ? fmtBytes(node.memory_used_bytes) + "/" + fmtBytes(node.memory_total_bytes)
        : "N/A"
    );
    s.stats.set(
      "guests",
      "GUESTS",
      (node.vms && node.vms.permitted === false
        ? "VM n/p"
        : "VM " + ((node.vms && node.vms.running) || 0) + "/" + ((node.vms && node.vms.total) || 0)) +
        "  " +
        (node.containers && node.containers.permitted === false
          ? "CT n/p"
          : "CT " +
            ((node.containers && node.containers.running) || 0) +
            "/" +
            ((node.containers && node.containers.total) || 0))
    );
    s.stats.set(
      "store",
      "STORE",
      isNum(node.storage_available_bytes) ? fmtBytes(node.storage_available_bytes) + " free" : "N/A"
    );
    s.stats.set("state", "SEEN", node.last_success ? fmtAgo(node.last_success) : "never", stale ? "warning" : "dim");

    if (status !== "ONLINE" && node.message) {
      s.stats.note(
        "hint",
        (stale ? "STALE since " + fmtClock(node.last_success) + " — " : "") + node.message,
        "warning"
      );
    } else {
      s.stats.hide("hint");
    }
  }

  function renderOverall(snapshot) {
    var el = document.getElementById("overall");
    var win = snapshot.windows || {};
    var primary = win.primary || {};
    var worst = "normal";

    ["cpu_percent", "memory_percent", "disk_percent"].forEach(function (k) {
      var state = usageState(primary[k]);
      if (state === "critical") worst = "critical";
      else if (state === "warning" && worst !== "critical") worst = "warning";
    });

    var temp = (win.temperature || {}).cpu_celsius;
    if (isNum(temp)) {
      var ts = tempState(temp);
      if (ts === "critical") worst = "critical";
      else if (ts === "warning" && worst !== "critical") worst = "warning";
    }

    var nodes = snapshot.proxmox || {};
    var troubled = 0;
    Object.keys(nodes).forEach(function (k) {
      var st = nodes[k].status;
      if (st === "OFFLINE" || st === "AUTH_ERROR" || st === "CONFIG_ERROR") troubled += 1;
    });
    if (troubled > 0 && worst !== "critical") worst = "warning";

    if (!everLoaded) {
      el.textContent = "WAITING FOR DATA";
      el.className = "overall waiting";
      return;
    }

    var text = "ALL SYSTEMS OPERATIONAL";
    if (worst === "critical") text = "CRITICAL";
    else if (worst === "warning") text = troubled > 0 ? "NODE ATTENTION" : "WARNING";

    el.textContent = text;
    el.className = "overall" + (worst === "normal" ? "" : " " + worst);
  }

  /* --------------------------------------------------------- detail view */

  var detailRows = {};

  function detailSet(listId, key, label, value, cls) {
    var id = listId + ":" + key;
    var row = detailRows[id];
    if (!row) {
      var list = document.getElementById(listId);
      var wrap = document.createElement("div");
      var dt = document.createElement("dt");
      var dd = document.createElement("dd");
      dt.textContent = label;
      wrap.appendChild(dt);
      wrap.appendChild(dd);
      list.appendChild(wrap);
      row = detailRows[id] = { dt: dt, dd: dd };
    }
    if (row.dt.textContent !== label) row.dt.textContent = label;
    if (row.dd.textContent !== value) row.dd.textContent = value;
    row.dd.className = cls || "";
  }

  var coreRows = [];

  function renderDetail(win) {
    var cpu = win.cpu || {};
    var load = cpu.load || {};
    var mem = win.memory || {};
    var swap = win.swap || {};
    var io = win.disk_io || {};
    var net = win.network || {};
    var proc = win.processes || {};
    var health = win.disk_health || {};

    var container = document.getElementById("cores");
    var cores = cpu.per_core || [];
    cores.forEach(function (core, i) {
      var row = coreRows[i];
      if (!row) {
        var el = document.createElement("div");
        el.className = "core";
        var n = document.createElement("span");
        n.className = "n";
        n.textContent = "C" + i;
        var bar = document.createElement("span");
        bar.className = "bar";
        var fill = document.createElement("span");
        fill.className = "fill";
        bar.appendChild(fill);
        var p = document.createElement("span");
        p.className = "p";
        el.appendChild(n);
        el.appendChild(bar);
        el.appendChild(p);
        container.appendChild(el);
        row = coreRows[i] = { fill: fill, p: p };
      }
      var pct = isNum(core.percent) ? Math.max(0, Math.min(100, core.percent)) : null;
      row.fill.style.width = pct === null ? "0%" : pct + "%";
      row.fill.className = "fill " + usageState(pct);
      row.p.textContent = fmtPct(core.percent);
    });

    detailSet("detail-cpu", "model", "Model", (win.system || {}).cpu_name || "N/A");
    detailSet("detail-cpu", "user", "User", fmtPct(cpu.user));
    detailSet("detail-cpu", "system", "System", fmtPct(cpu.system));
    detailSet("detail-cpu", "idle", "Idle", fmtPct(cpu.idle));
    detailSet("detail-cpu", "dpc", "DPC / IRQ", fmtPct(cpu.dpc) + " / " + fmtPct(cpu.interrupt));
    detailSet(
      "detail-cpu",
      "load",
      "Load 1/5/15",
      isNum(load.min1) ? load.min1 + " " + load.min5 + " " + load.min15 : "N/A",
      "dim"
    );
    detailSet(
      "detail-cpu",
      "ctx",
      "Ctx / Int per s",
      (isNum(cpu.ctx_switches_per_sec) ? cpu.ctx_switches_per_sec : "N/A") +
        " / " +
        (isNum(cpu.interrupts_per_sec) ? cpu.interrupts_per_sec : "N/A")
    );

    detailSet("detail-mem", "total", "Total", fmtBytes(mem.total_bytes));
    detailSet("detail-mem", "used", "Used", fmtBytes(mem.used_bytes), usageState(mem.percent));
    detailSet("detail-mem", "avail", "Available", fmtBytes(mem.available_bytes));
    detailSet("detail-mem", "pct", "Used %", fmtPct(mem.percent), usageState(mem.percent));
    detailSet(
      "detail-mem",
      "swap",
      "Swap",
      isNum(swap.used_bytes) ? fmtBytes(swap.used_bytes) + " / " + fmtBytes(swap.total_bytes) : "N/A"
    );
    detailSet("detail-mem", "swappct", "Swap %", fmtPct(swap.percent));

    detailSet("detail-proc", "total", "Processes", isNum(proc.total) ? String(proc.total) : "N/A");
    detailSet("detail-proc", "threads", "Threads", isNum(proc.threads) ? String(proc.threads) : "N/A");

    (win.filesystems || []).forEach(function (fs, i) {
      detailSet(
        "detail-disk",
        "fs" + i,
        fs.mount,
        fs.present
          ? fmtBytes(fs.used_bytes) + " / " + fmtBytes(fs.total_bytes) + "  " + fmtPct(fs.percent)
          : "NOT PRESENT",
        fs.present ? usageState(fs.percent) : "dim"
      );
    });
    detailSet("detail-disk", "read", "Read", fmtRate(io.read_bytes_per_sec));
    detailSet("detail-disk", "write", "Write", fmtRate(io.write_bytes_per_sec));
    (io.per_disk || []).forEach(function (d, i) {
      detailSet(
        "detail-disk",
        "pd" + i,
        d.name,
        "R " + fmtRate(d.read_bytes_per_sec) + " W " + fmtRate(d.write_bytes_per_sec),
        "dim"
      );
    });
    (health.disks || []).forEach(function (d, i) {
      detailSet(
        "detail-disk",
        "hd" + i,
        (d.name || "disk") + " " + (d.media_type || ""),
        (d.health || "N/A") + " / " + (d.operational || "N/A"),
        d.health && d.health.toLowerCase() === "healthy" ? "good" : "warning"
      );
    });
    if (!health.available && health.detail) {
      detailSet("detail-disk", "hnote", "Drive health", health.detail, "dim");
    }

    detailSet("detail-net", "iface", "Interface", net.interface || "N/A");
    detailSet(
      "detail-net",
      "ip",
      "IPv4",
      net.ip_address ? net.ip_address + "/" + (net.ip_mask_cidr || "?") : "N/A"
    );
    detailSet("detail-net", "link", "Link", net.connectivity || "UNKNOWN",
      net.connectivity === "LINK UP" ? "good" : "warning");
    detailSet("detail-net", "speed", "Speed", isNum(net.speed_mbps) ? net.speed_mbps + " Mb/s" : "N/A");
    detailSet("detail-net", "down", "Down", fmtRate(net.download_bytes_per_sec));
    detailSet("detail-net", "up", "Up", fmtRate(net.upload_bytes_per_sec));
    detailSet("detail-net", "rx", "Total RX", fmtBytes(net.total_bytes_recv));
    detailSet("detail-net", "tx", "Total TX", fmtBytes(net.total_bytes_sent));
    (net.per_interface || [])
      .filter(function (i) {
        return i.up && i.name.toLowerCase().indexOf("loopback") === -1;
      })
      .slice(0, 4)
      .forEach(function (i, idx) {
        detailSet(
          "detail-net",
          "nic" + idx,
          i.name.slice(0, 18),
          "↓" + fmtRate(i.download_bytes_per_sec) + " ↑" + fmtRate(i.upload_bytes_per_sec),
          "dim"
        );
      });
  }

  /* ------------------------------------------------------------- transport */

  function showBanner(title, text) {
    document.getElementById("banner-title").textContent = title;
    document.getElementById("banner-text").textContent = text;
    document.getElementById("banner").hidden = false;
  }

  function hideBanner() {
    document.getElementById("banner").hidden = true;
  }

  /** Report why the API could not be read, in terms that name the fix. */
  function noteApiFailure(path, status, immediate) {
    apiFailures += 1;
    // One miss can be a restart; two means the endpoint is really not there.
    // A 404 needs no second opinion: the route simply does not exist.
    if (apiFailures < 2 && !immediate && status !== 404) return;

    // The header must stop saying STARTING once the first attempts have failed.
    var overall = document.getElementById("overall");
    overall.textContent = "NO DATA";
    overall.className = "overall critical";

    var origin = window.location.origin;
    if (status === 404) {
      showBanner(
        "WRONG SERVER ON THIS PORT",
        origin + path + " returned 404. Something other than PLH Rack Monitor " +
          "is serving this port - start the backend, or open the port it is listening on."
      );
    } else if (status) {
      showBanner("BACKEND ERROR", origin + path + " returned HTTP " + status + ".");
    } else {
      showBanner(
        "BACKEND UNREACHABLE",
        "No response from " + origin + path + ". Start it with start_monitor.ps1."
      );
    }
  }

  /** GET JSON, rejecting with the status code so a 404 can be explained. */
  function getJson(path) {
    return fetch(path, { cache: "no-store" }).then(function (response) {
      if (!response.ok) {
        var error = new Error("HTTP " + response.status);
        error.status = response.status;
        throw error;
      }
      return response.json();
    });
  }

  function applySnapshot(snapshot) {
    lastSnapshot = snapshot;
    lastFrameAt = Date.now();
    everLoaded = true;
    apiFailures = 0;
    hideBanner();
    renderWindows(snapshot.windows || {}, snapshot.service);
    Object.keys(sections).forEach(function (key) {
      if (key === "windows") return;
      renderNode(key, (snapshot.proxmox || {})[key]);
    });
    renderOverall(snapshot);
    if (detailOpen) renderDetail(snapshot.windows || {});
  }

  function setLink(state, text) {
    var el = document.getElementById("link-state");
    el.className = "link-state " + state;
    el.textContent = text;
  }

  function startPolling() {
    if (pollTimer) return;
    setLink("polling", "POLLING");
    pollTimer = setInterval(function () {
      getJson("/api/metrics")
        .then(applySnapshot)
        .catch(function (error) {
          setLink("down", "NO DATA");
          noteApiFailure("/api/metrics", error.status);
        });
    }, 2000);
  }

  function stopPolling() {
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
  }

  function connectStream() {
    if (!window.EventSource) {
      startPolling();
      return;
    }
    try {
      eventSource = new EventSource("/api/stream");
    } catch (e) {
      startPolling();
      return;
    }

    eventSource.addEventListener("open", function () {
      stopPolling();
      setLink("live", "LIVE");
    });

    eventSource.addEventListener("metrics", function (event) {
      try {
        applySnapshot(JSON.parse(event.data));
        stopPolling();
        setLink("live", "LIVE");
      } catch (e) {
        /* A malformed frame is skipped; the next one is a full snapshot. */
      }
    });

    eventSource.addEventListener("error", function () {
      // EventSource retries on its own; polling covers the gap meanwhile.
      setLink("polling", "RECONNECTING");
      startPolling();
    });
  }

  /* ------------------------------------------------------------ view mode */

  var VIEWS = ["all", "windows", "node1", "node2"];
  var currentView = "all";

  function applyView(view) {
    if (VIEWS.indexOf(view) === -1) view = "all";
    currentView = view;

    var panels = document.getElementById("panels");
    panels.className = "panels" + (view === "all" ? "" : " solo");

    // A hidden panel keeps its donut objects, so returning to it does not
    // rebuild anything and its charts continue from their current values.
    Object.keys(sections).forEach(function (key) {
      sections[key].panel.hidden = view !== "all" && view !== key;
    });

    Array.prototype.forEach.call(document.querySelectorAll(".view-btn"), function (btn) {
      btn.classList.toggle("active", btn.getAttribute("data-view") === view);
    });

    try {
      localStorage.setItem("plh.view", view);
    } catch (e) {
      /* Private mode or blocked storage: the choice simply is not remembered. */
    }
  }

  function installViews() {
    Array.prototype.forEach.call(document.querySelectorAll(".view-btn"), function (btn) {
      btn.addEventListener("click", function () {
        applyView(btn.getAttribute("data-view"));
      });
    });

    // Clicking a panel in the combined view isolates it; clicking again
    // returns to all three.
    Object.keys(sections).forEach(function (key) {
      sections[key].panel.addEventListener("click", function () {
        applyView(currentView === key ? "all" : key);
      });
    });

    var stored = null;
    try {
      stored = localStorage.getItem("plh.view");
    } catch (e) {
      stored = null;
    }
    applyView(stored || "all");
  }

  /* ------------------------------------------------------------ fullscreen */

  function toggleFullscreen() {
    if (document.fullscreenElement) {
      if (document.exitFullscreen) document.exitFullscreen();
    } else if (document.documentElement.requestFullscreen) {
      document.documentElement.requestFullscreen().catch(function () {});
    }
  }

  function installFullscreen() {
    document.getElementById("fs-btn").addEventListener("click", toggleFullscreen);

    document.addEventListener("keydown", function (event) {
      if (event.key === "F11") {
        // Native F11 is not blocked. If the browser handled it, the viewport
        // changes and nothing more is done; if it did not, the API is used.
        var wasFullscreen = !!document.fullscreenElement;
        var height = window.innerHeight;
        setTimeout(function () {
          var changed =
            !!document.fullscreenElement !== wasFullscreen || window.innerHeight !== height;
          if (!changed) toggleFullscreen();
        }, 300);
        return;
      }
      if (event.key === "d" || event.key === "D") {
        setDetail(!detailOpen);
      } else if (event.key === "Escape" && detailOpen) {
        setDetail(false);
      } else if (event.key === "0" || event.key === "a" || event.key === "A") {
        applyView("all");
      } else if (event.key === "1") {
        applyView("windows");
      } else if (event.key === "2") {
        applyView("node1");
      } else if (event.key === "3") {
        applyView("node2");
      }
    });
  }

  function setDetail(open) {
    detailOpen = open;
    document.getElementById("detail").hidden = !open;
    if (open && lastSnapshot) renderDetail(lastSnapshot.windows || {});
  }

  /* ------------------------------------------------------------------ init */

  function init() {
    buildSection("windows");
    buildSection("node1");
    buildSection("node2");

    fitStage();
    window.addEventListener("resize", fitStage);
    document.addEventListener("fullscreenchange", fitStage);

    installFullscreen();
    installViews();
    document.getElementById("detail-btn").addEventListener("click", function (event) {
      // The button sits outside the panels, so this never doubles as a panel
      // click that would also change the view.
      event.stopPropagation();
      setDetail(!detailOpen);
    });

    getJson("/api/config")
      .then(function (cfg) {
        if (cfg && cfg.thresholds) config.thresholds = cfg.thresholds;
        if (cfg && cfg.nodes) {
          cfg.nodes.forEach(function (node) {
            var s = sections[node.key];
            if (s && node.name) s.title.textContent = node.name;
          });
        }
      })
      .catch(function (error) {
        /* Threshold defaults are already loaded, so the dashboard still runs,
           but a 404 here is the clearest sign of the wrong server. */
        noteApiFailure("/api/config", error.status, true);
      })
      .then(function () {
        connectStream();
        // A first paint without waiting for the initial stream push.
        getJson("/api/metrics")
          .then(applySnapshot)
          .catch(function (error) {
            noteApiFailure("/api/metrics", error.status);
          });
      });

    // A stream that stops delivering is surfaced rather than left looking live.
    setInterval(function () {
      if (lastFrameAt && Date.now() - lastFrameAt > 8000) {
        setLink("down", "STALLED");
      }
    }, 2000);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();

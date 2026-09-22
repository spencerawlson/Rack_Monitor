/* PLH Rack Monitor - dashboard client.
 *
 * Panels are generated from /api/config: one for this machine, one per
 * configured Proxmox node. The grid is chosen to show them as large as the
 * window allows; when there are more machines than fit legibly, the combined
 * view pages through them.
 *
 * Elements are created once and updated in place. Nothing is rebuilt with
 * innerHTML on a tick, because replacing a node restarts its CSS transition
 * and the charts would jump instead of glide.
 *
 * Live data arrives over Server-Sent Events, with polling as a fallback.
 */

(function () {
  "use strict";

  var SVG_NS = "http://www.w3.org/2000/svg";
  var RADIUS = 40;
  var CIRCUMFERENCE = 2 * Math.PI * RADIUS;

  // The reference panel: one of three on a 1424 x 280 strip.
  var BASE_W = 464;
  var BASE_H = 230;
  // Below this scale text becomes too small to read on a small panel, so
  // the combined view pages instead of shrinking further.
  var MIN_SCALE = 0.85;

  var config = {
    display: { title: "PLH RACK MONITOR", page_seconds: 10 },
    thresholds: { usage: { warning: 70, critical: 90 }, temperature: { warning: 75, critical: 90 } },
    host: { label: "" },
    nodes: [],
    config_warnings: []
  };

  var sections = {};
  var order = [];
  var currentView = "all";
  var pages = [[]];
  var page = 0;
  var pageTimer = null;
  var detailOpen = false;
  var lastSnapshot = null;
  var pollTimer = null;
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

    function circle(cls) {
      var c = document.createElementNS(SVG_NS, "circle");
      c.setAttribute("class", cls);
      c.setAttribute("cx", "50");
      c.setAttribute("cy", "50");
      c.setAttribute("r", String(RADIUS));
      return c;
    }
    var track = circle("track");
    var arc = circle("arc");
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
      // The ring is animated by CSS; only the number is stepped so the
      // digits do not snap while the arc is still travelling.
      if (animation) cancelAnimationFrame(animation);
      var from = shown === null ? next : shown;
      var start = performance.now();
      function step(now) {
        var t = Math.min(1, (now - start) / 600);
        paint(from + (next - from) * (1 - Math.pow(1 - t, 3)));
        animation = t < 1 ? requestAnimationFrame(step) : null;
      }
      animation = requestAnimationFrame(step);
    }

    return {
      set: function (percent, stale) {
        svg.setAttribute("class", "donut " + usageState(percent) + (stale ? " stale" : ""));
        if (!isNum(percent)) {
          if (animation) cancelAnimationFrame(animation);
          animation = null;
          shown = null;
          value.textContent = "N/A";
          arc.setAttribute("stroke-dashoffset", String(CIRCUMFERENCE));
          return;
        }
        var clamped = Math.max(0, Math.min(100, percent));
        arc.setAttribute("stroke-dashoffset", String(CIRCUMFERENCE * (1 - clamped / 100)));
        if (shown === null) paint(clamped);
        else if (Math.abs(clamped - shown) >= 0.5) tweenTo(clamped);
        shown = clamped;
      }
    };
  }

  /* ----------------------------------------------------------- sparklines */

  /** The path for one series, oldest point at the left edge.
   *
   *  Nulls break the line rather than being drawn through: a gap in
   *  collection is not a dip to zero. A reading with no neighbour becomes a
   *  zero-length segment, which the round cap renders as a dot, so a single
   *  point is still visible.
   */
  function sparkPath(points) {
    var count = points.length;
    var d = "";
    var run = [];

    function flush() {
      if (!run.length) return;
      d += "M" + run[0] + "L" + run[run.length === 1 ? 0 : 1];
      for (var k = 2; k < run.length; k++) d += "L" + run[k];
      run = [];
    }

    for (var i = 0; i < count; i++) {
      if (!isNum(points[i])) {
        flush();
        continue;
      }
      var x = count > 1 ? (i / (count - 1)) * 100 : 50;
      // A 24 unit box with a unit of padding, so a reading at 0% or 100%
      // is not clipped by its own stroke width.
      var y = 23 - (Math.max(0, Math.min(100, points[i])) / 100) * 22;
      run.push(x.toFixed(2) + " " + y.toFixed(2));
    }
    flush();
    return d;
  }

  /** Two readings are the fewest that can show a direction. */
  function drawable(points) {
    var found = 0;
    for (var i = 0; i < (points || []).length; i++) {
      if (isNum(points[i]) && ++found === 2) return true;
    }
    return false;
  }

  function makeSpark(parent) {
    var svg = document.createElementNS(SVG_NS, "svg");
    svg.setAttribute("viewBox", "0 0 100 24");
    // Stretched to the panel width; the stroke is held constant in CSS.
    svg.setAttribute("preserveAspectRatio", "none");
    svg.setAttribute("class", "spark unavailable");

    var path = document.createElementNS(SVG_NS, "path");
    path.setAttribute("class", "line");
    svg.appendChild(path);
    parent.appendChild(svg);

    return {
      set: function (points, stale) {
        var list = points || [];
        var latest = null;
        for (var i = list.length - 1; i >= 0 && latest === null; i--) {
          if (isNum(list[i])) latest = list[i];
        }
        // The chart takes its colour from the newest reading, so it always
        // agrees with the ring above it.
        var cls = latest === null ? "spark unavailable" : "spark " + usageState(latest) + (stale ? " stale" : "");
        if (svg.getAttribute("class") !== cls) svg.setAttribute("class", cls);
        var d = sparkPath(list);
        if (path.getAttribute("d") !== d) path.setAttribute("d", d);
      }
    };
  }

  /** Draw a panel's three charts, or hide the row when there is no window
   *  worth drawing - an unconfigured node, or history switched off. */
  function applyHistory(s, history, stale) {
    if (!s.sparks) return;
    var h = history || {};
    var show = drawable(h.cpu) || drawable(h.memory) || drawable(h.disk);
    s.sparkRow.hidden = !show;
    if (!show) return;
    s.sparks.cpu.set(h.cpu, stale);
    s.sparks.memory.set(h.memory, stale);
    s.sparks.disk.set(h.disk, stale);
  }

  /* ---------------------------------------------------------- stat blocks */

  function StatBlock(container) {
    this.container = container;
    this.rows = {};
  }

  StatBlock.prototype.set = function (key, label, value, cls) {
    var row = this.rows[key];
    if (!row) {
      var el = document.createElement("div");
      el.className = "stat";
      var k = document.createElement("span");
      k.className = "k";
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
      this.container.appendChild(el);
      row = this.rows[key] = { el: el };
    }
    if (row.el.textContent !== text) row.el.textContent = text;
    row.el.className = "note" + (cls ? " " + cls : "");
    row.el.hidden = false;
  };

  StatBlock.prototype.hide = function (key) {
    var row = this.rows[key];
    if (row) row.el.hidden = true;
  };

  /* --------------------------------------------------------------- panels */

  function hostLabel() {
    var sys = lastSnapshot && lastSnapshot.host && lastSnapshot.host.system;
    return (config.host && config.host.label) || (sys && (sys.label || sys.hostname)) || "HOST";
  }

  function nodeConfig(key) {
    for (var i = 0; i < config.nodes.length; i++) {
      if (config.nodes[i].key === key) return config.nodes[i];
    }
    return null;
  }

  function shortLabel(key) {
    if (key === "host") return hostLabel().toUpperCase().slice(0, 12);
    var node = nodeConfig(key);
    return ((node && node.name) || key).toUpperCase().slice(0, 12);
  }

  function buildPanels() {
    var template = document.getElementById("panel-template");
    var container = document.getElementById("panels");
    container.textContent = "";
    sections = {};
    order = ["host"].concat(config.nodes.map(function (n) { return n.key; }));

    order.forEach(function (key) {
      var panel = template.content.firstElementChild.cloneNode(true);
      panel.id = "panel-" + key;
      container.appendChild(panel);
      var row = panel.querySelector(".donut-row");
      var sparkRow = panel.querySelector(".spark-row");
      sections[key] = {
        panel: panel,
        title: panel.querySelector("h2"),
        pill: panel.querySelector(".pill"),
        stats: new StatBlock(panel.querySelector(".stats")),
        donuts: { cpu: makeDonut(row, "CPU"), memory: makeDonut(row, "RAM"), disk: makeDonut(row, "DISK") },
        sparkRow: sparkRow,
        sparks: { cpu: makeSpark(sparkRow), memory: makeSpark(sparkRow), disk: makeSpark(sparkRow) }
      };
      sections[key].title.textContent = key === "host" ? hostLabel() : ((nodeConfig(key) || {}).name || key);
      // Clicking a panel isolates it; clicking again returns to all.
      panel.addEventListener("click", function () {
        applyView(currentView === key ? "all" : key);
      });
    });
    buildViewButtons();
  }

  function buildViewButtons() {
    var views = document.getElementById("views");
    views.textContent = "";
    var entries = [{ key: "all", label: "ALL" }].concat(
      order.map(function (key) { return { key: key, label: shortLabel(key) }; })
    );
    entries.forEach(function (entry, i) {
      var button = document.createElement("button");
      button.type = "button";
      button.className = "view-btn" + (entry.key === currentView ? " active" : "");
      button.setAttribute("data-view", entry.key);
      button.textContent = entry.label;
      button.title = i === 0 ? "All machines (0)" : entry.label + (i <= 9 ? " (" + i + ")" : "");
      button.addEventListener("click", function (event) {
        event.stopPropagation();
        applyView(entry.key);
      });
      views.appendChild(button);
    });
  }

  /* --------------------------------------------------------------- layout */

  /** The column count that shows `n` panels largest in a W x H area. */
  function bestGrid(n, width, height, gap) {
    var best = null;
    for (var cols = 1; cols <= Math.max(1, n); cols++) {
      var rows = Math.ceil(n / cols);
      var pw = (width - gap * (cols - 1)) / cols;
      var ph = (height - gap * (rows - 1)) / rows;
      var scale = Math.min(pw / BASE_W, ph / BASE_H);
      var empty = cols * rows - n;
      var score = scale - empty * 0.02;
      if (!best || score > best.score + 1e-9) {
        best = { cols: cols, rows: rows, scale: scale, empty: empty, score: score };
      }
    }
    return best;
  }

  function areaOf(container) {
    var style = getComputedStyle(container);
    return {
      width: container.clientWidth,
      height: container.clientHeight,
      gap: parseFloat(style.columnGap) || 8
    };
  }

  function paginate(keys, area) {
    for (var per = keys.length; per >= 1; per--) {
      if (per === 1 || bestGrid(per, area.width, area.height, area.gap).scale >= MIN_SCALE) {
        var out = [];
        for (var i = 0; i < keys.length; i += per) out.push(keys.slice(i, i + per));
        return out;
      }
    }
    return [keys];
  }

  function layout() {
    var container = document.getElementById("panels");
    var area = areaOf(container);
    var keys = currentView === "all" ? order.slice() : [currentView];
    pages = currentView === "all" ? paginate(keys, area) : [keys];
    if (page >= pages.length) page = 0;
    showPage(area);
    schedulePaging();
  }

  function showPage(area) {
    var container = document.getElementById("panels");
    area = area || areaOf(container);
    var keys = pages[page] || [];
    var grid = bestGrid(keys.length, area.width, area.height, area.gap);
    container.style.gridTemplateColumns = "repeat(" + grid.cols + ", minmax(0, 1fr))";
    container.style.gridTemplateRows = "repeat(" + grid.rows + ", minmax(0, 1fr))";
    order.forEach(function (key) {
      var panel = sections[key].panel;
      panel.hidden = keys.indexOf(key) === -1;
      panel.style.gridColumn = "";
    });
    // Spare cells in the last row are given to the first panel instead of
    // being left as holes.
    if (grid.empty > 0 && keys.length > 0) {
      sections[keys[0]].panel.style.gridColumn = "span " + (1 + grid.empty);
    }
    var pager = document.getElementById("pager");
    pager.hidden = pages.length < 2;
    pager.textContent = "PAGE " + (page + 1) + "/" + pages.length;
  }

  function schedulePaging() {
    if (pageTimer) clearInterval(pageTimer);
    pageTimer = null;
    if (pages.length > 1) {
      var ms = Math.max(2, Number(config.display.page_seconds) || 10) * 1000;
      pageTimer = setInterval(function () { turnPage(1); }, ms);
    }
  }

  function turnPage(delta) {
    if (pages.length < 2) return;
    page = (page + delta + pages.length) % pages.length;
    showPage();
  }

  var VIEW_KEY = "plh.view";

  function applyView(view) {
    if (view !== "all" && !sections[view]) view = "all";
    currentView = view;
    page = 0;
    Array.prototype.forEach.call(document.querySelectorAll(".view-btn"), function (b) {
      b.classList.toggle("active", b.getAttribute("data-view") === view);
    });
    layout();
    try {
      localStorage.setItem(VIEW_KEY, view);
    } catch (e) {
      /* Private mode or blocked storage: the choice is simply not kept. */
    }
  }

  /* ------------------------------------------------------------ rendering */

  function renderHost(host, service) {
    var s = sections.host;
    if (!s) return;
    var sys = host.system || {};
    var cpu = host.cpu || {};
    var mem = host.memory || {};
    var net = host.network || {};
    var temp = host.temperature || {};
    var health = host.disk_health || {};
    var primary = host.primary || {};
    var io = host.disk_io || {};

    var title = hostLabel();
    if (s.title.textContent !== title) {
      s.title.textContent = title;
      var button = document.querySelector('.view-btn[data-view="host"]');
      if (button) button.textContent = shortLabel("host");
    }

    var degraded = service && service.status === "DEGRADED";
    s.pill.textContent = degraded ? "DEGRADED" : "ONLINE";
    s.pill.className = "pill " + (degraded ? "warning" : "online");

    s.donuts.cpu.set(primary.cpu_percent, false);
    s.donuts.memory.set(primary.memory_percent, false);
    s.donuts.disk.set(primary.disk_percent, false);
    applyHistory(s, host.history, false);

    var tState = tempState(temp.cpu_celsius);
    s.stats.set("temp", "TEMP", isNum(temp.cpu_celsius) ? temp.cpu_celsius.toFixed(1) + "°C" : "N/A",
      tState === "unavailable" ? "dim" : tState);
    s.stats.set("uptime", "UP", fmtUptime(sys.uptime_seconds));
    s.stats.set("net", "NET", "↓" + fmtRate(net.download_bytes_per_sec) + " ↑" + fmtRate(net.upload_bytes_per_sec));
    s.stats.set("io", "I/O", "R " + fmtRate(io.read_bytes_per_sec) + " W " + fmtRate(io.write_bytes_per_sec));
    s.stats.set("disk", "VOL", (primary.disk_mount || "?") + " " + fmtPct(primary.disk_percent));
    s.stats.set("ram", "RAM", isNum(mem.used_bytes) ? fmtBytes(mem.used_bytes) + "/" + fmtBytes(mem.total_bytes) : "N/A");
    s.stats.set("cores", "CPU",
      (isNum(cpu.cores_physical) ? cpu.cores_physical + "C" : "?") + "/" +
      (isNum(cpu.cores_logical) ? cpu.cores_logical + "T" : "?") +
      (isNum(cpu.freq_current_mhz) ? " " + Math.round(cpu.freq_current_mhz) + "MHz" : ""));

    var disks = health.disks || [];
    var unhealthy = disks.filter(function (d) {
      return d.health && d.health.toLowerCase() !== "healthy";
    });
    s.stats.set("smart", "DISKS",
      health.available ? disks.length + " " + (unhealthy.length ? unhealthy.length + " DEGRADED" : "HEALTHY") : "N/A",
      health.available ? (unhealthy.length ? "critical" : "good") : "dim");

    if (!temp.available && temp.detail) s.stats.note("tempnote", "TEMP: " + temp.detail, null);
    else s.stats.hide("tempnote");
  }

  /** "VM 1/2", "VM n/p" when the token may not look, "VM N/A" when the
   *  count could not be read - never 0/0 standing in for an unknown. */
  function guestCount(label, guests) {
    if (guests && guests.permitted === false) return label + " n/p";
    if (!guests || !isNum(guests.total)) return label + " N/A";
    return label + " " + (isNum(guests.running) ? guests.running : 0) + "/" + guests.total;
  }

  function renderNode(node) {
    var s = sections[node.key];
    if (!s) return;
    var status = node.status || "UNKNOWN";
    var stale = !!node.stale;
    var primary = node.primary || {};

    s.title.textContent = node.name || node.key;
    var unconfigured = status === "UNCONFIGURED" || status === "CONFIG_ERROR";
    s.panel.classList.toggle("unconfigured", unconfigured);
    s.panel.classList.toggle("offline", status === "OFFLINE" || status === "AUTH_ERROR");

    s.pill.textContent = stale ? status + " · STALE" : status;
    s.pill.className = "pill " + (
      status === "ONLINE" ? "online"
        : status === "UNCONFIGURED" || status === "CONNECTING" ? "unconfigured"
          : status === "DEV_MOCK" ? "warning"
            : "offline");

    // An unconfigured node shows no figures at all: empty rings, never a
    // plausible looking percentage.
    s.donuts.cpu.set(primary.cpu_percent, stale);
    s.donuts.memory.set(primary.memory_percent, stale);
    s.donuts.disk.set(primary.disk_percent, stale);
    // An unconfigured node has only nulls, so its row stays hidden.
    applyHistory(s, node.history, stale);

    if (unconfigured || status === "CONNECTING") {
      s.stats.set("state", "STATUS", unconfigured ? "NOT CONFIGURED" : "WAITING FOR FIRST REPLY", "dim");
      s.stats.set("temp", "TEMP", "N/A", "dim");
      ["uptime", "guests", "store", "ram"].forEach(function (k) { s.stats.hide(k); });
      if (unconfigured) {
        s.stats.note("hint", (node.message || "Add host and API token") + " — see config.toml", null);
      } else {
        s.stats.hide("hint");
      }
      return;
    }

    s.stats.set("temp", "TEMP", "N/A", "dim");
    s.stats.set("uptime", "UP", fmtUptime(node.uptime_seconds));
    s.stats.set("ram", "RAM", isNum(node.memory_used_bytes)
      ? fmtBytes(node.memory_used_bytes) + "/" + fmtBytes(node.memory_total_bytes) : "N/A");
    s.stats.set("guests", "GUESTS", guestCount("VM", node.vms) + "  " + guestCount("CT", node.containers));
    s.stats.set("store", "STORE", isNum(node.storage_available_bytes) ? fmtBytes(node.storage_available_bytes) + " free" : "N/A");
    s.stats.set("state", "SEEN", node.last_success ? fmtAgo(node.last_success) : "never", stale ? "warning" : "dim");

    if (status !== "ONLINE" && node.message) {
      s.stats.note("hint", (stale ? "STALE since " + fmtClock(node.last_success) + " — " : "") + node.message, "warning");
    } else {
      s.stats.hide("hint");
    }
  }

  function renderOverall(snapshot) {
    var el = document.getElementById("overall");
    if (!everLoaded) {
      el.textContent = "WAITING FOR DATA";
      el.className = "overall waiting";
      return;
    }
    var host = snapshot.host || {};
    var primary = host.primary || {};
    var worst = "normal";
    function consider(state) {
      if (state === "critical") worst = "critical";
      else if (state === "warning" && worst !== "critical") worst = "warning";
    }
    ["cpu_percent", "memory_percent", "disk_percent"].forEach(function (k) { consider(usageState(primary[k])); });
    var temp = (host.temperature || {}).cpu_celsius;
    if (isNum(temp)) consider(tempState(temp));

    var troubled = 0;
    (snapshot.nodes || []).forEach(function (node) {
      if (node.status === "OFFLINE" || node.status === "AUTH_ERROR" || node.status === "CONFIG_ERROR") troubled += 1;
    });
    if (troubled > 0) consider("warning");

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
      var wrap = document.createElement("div");
      var dt = document.createElement("dt");
      var dd = document.createElement("dd");
      wrap.appendChild(dt);
      wrap.appendChild(dd);
      document.getElementById(listId).appendChild(wrap);
      row = detailRows[id] = { wrap: wrap, dt: dt, dd: dd };
    }
    if (row.dt.textContent !== label) row.dt.textContent = label;
    if (row.dd.textContent !== value) row.dd.textContent = value;
    row.dd.className = cls || "";
    row.wrap.hidden = false;
  }

  /** Hide rows of a list section before it is redrawn, so entries that
   *  disappeared (a detached disk, a removed adapter) do not linger. */
  function detailHide(listId, prefix) {
    var start = listId + ":" + prefix;
    Object.keys(detailRows).forEach(function (id) {
      if (id.indexOf(start) === 0) detailRows[id].wrap.hidden = true;
    });
  }

  var coreRows = [];

  function renderDetail(host) {
    var sys = host.system || {};
    var cpu = host.cpu || {};
    var load = cpu.load || {};
    var mem = host.memory || {};
    var swap = host.swap || {};
    var io = host.disk_io || {};
    var net = host.network || {};
    var proc = host.processes || {};
    var health = host.disk_health || {};

    document.getElementById("detail-title").textContent =
      hostLabel().toUpperCase() + (sys.os_version ? " · " + sys.os_version : "") + " — DETAIL";

    var cores = cpu.per_core || [];
    var container = document.getElementById("cores");
    var columns = cores.length <= 8 ? 1 : cores.length <= 16 ? 2 : cores.length <= 32 ? 4 : 8;
    container.style.gridTemplateColumns = "repeat(" + columns + ", minmax(0, 1fr))";
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

    detailSet("detail-cpu", "model", "Model", sys.cpu_name || "N/A");
    detailSet("detail-cpu", "user", "User", fmtPct(cpu.user));
    detailSet("detail-cpu", "system", "System", fmtPct(cpu.system));
    detailSet("detail-cpu", "idle", "Idle", fmtPct(cpu.idle));
    // Each platform reports its own extra states; only those present are shown.
    if (isNum(cpu.dpc) || isNum(cpu.interrupt)) {
      detailSet("detail-cpu", "extra", "DPC / IRQ", fmtPct(cpu.dpc) + " / " + fmtPct(cpu.interrupt));
    } else if (isNum(cpu.iowait) || isNum(cpu.irq)) {
      detailSet("detail-cpu", "extra", "IOWait / IRQ",
        fmtPct(cpu.iowait) + " / " + fmtPct((cpu.irq || 0) + (cpu.softirq || 0)));
    }
    detailSet("detail-cpu", "load", "Load 1/5/15",
      load.available && isNum(load.min1) ? load.min1 + " " + load.min5 + " " + load.min15 : "N/A", "dim");
    detailSet("detail-cpu", "ctx", "Ctx / Int per s",
      (isNum(cpu.ctx_switches_per_sec) ? cpu.ctx_switches_per_sec : "N/A") + " / " +
      (isNum(cpu.interrupts_per_sec) ? cpu.interrupts_per_sec : "N/A"));

    detailSet("detail-mem", "total", "Total", fmtBytes(mem.total_bytes));
    detailSet("detail-mem", "used", "Used", fmtBytes(mem.used_bytes), usageState(mem.percent));
    detailSet("detail-mem", "avail", "Available", fmtBytes(mem.available_bytes));
    detailSet("detail-mem", "pct", "Used %", fmtPct(mem.percent), usageState(mem.percent));
    detailSet("detail-mem", "swap", "Swap",
      isNum(swap.used_bytes) ? fmtBytes(swap.used_bytes) + " / " + fmtBytes(swap.total_bytes) : "N/A");
    detailSet("detail-mem", "swappct", "Swap %", fmtPct(swap.percent));

    detailSet("detail-proc", "total", "Processes", isNum(proc.total) ? String(proc.total) : "N/A");
    detailSet("detail-proc", "threads", "Threads", isNum(proc.threads) ? String(proc.threads) : "N/A");
    if (isNum(proc.running)) detailSet("detail-proc", "running", "Running", String(proc.running));

    detailHide("detail-disk", "");
    (host.filesystems || []).forEach(function (fs, i) {
      detailSet("detail-disk", "fs" + i, fs.mount + (fs.kind ? " " + fs.kind : ""),
        fs.present ? fmtBytes(fs.used_bytes) + " / " + fmtBytes(fs.total_bytes) + "  " + fmtPct(fs.percent) : "NOT PRESENT",
        fs.present ? usageState(fs.percent) : "dim");
    });
    detailSet("detail-disk", "read", "Read", fmtRate(io.read_bytes_per_sec));
    detailSet("detail-disk", "write", "Write", fmtRate(io.write_bytes_per_sec));
    (io.per_disk || []).forEach(function (d, i) {
      detailSet("detail-disk", "pd" + i, d.name,
        "R " + fmtRate(d.read_bytes_per_sec) + " W " + fmtRate(d.write_bytes_per_sec), "dim");
    });
    (health.disks || []).forEach(function (d, i) {
      detailSet("detail-disk", "hd" + i, (d.name || "disk") + " " + (d.media_type || ""),
        (d.health || "N/A") + " / " + (d.operational || "N/A"),
        d.health && d.health.toLowerCase() === "healthy" ? "good" : "warning");
    });
    if (!health.available && health.detail) {
      detailSet("detail-disk", "hnote", "Drive health", health.detail, "dim");
    }

    detailHide("detail-net", "nic");
    detailSet("detail-net", "iface", "Interface", net.interface || "N/A");
    detailSet("detail-net", "ip", "IPv4", net.ip_address ? net.ip_address + "/" + (net.ip_mask_cidr || "?") : "N/A");
    detailSet("detail-net", "link", "Link", net.connectivity || "UNKNOWN", net.connectivity === "LINK UP" ? "good" : "warning");
    detailSet("detail-net", "speed", "Speed", isNum(net.speed_mbps) ? net.speed_mbps + " Mb/s" : "N/A");
    detailSet("detail-net", "down", "Down", fmtRate(net.download_bytes_per_sec));
    detailSet("detail-net", "up", "Up", fmtRate(net.upload_bytes_per_sec));
    detailSet("detail-net", "rx", "Total RX", fmtBytes(net.total_bytes_recv));
    detailSet("detail-net", "tx", "Total TX", fmtBytes(net.total_bytes_sent));
    (net.per_interface || [])
      .filter(function (i) {
        return i.up !== false && i.name.toLowerCase().indexOf("loopback") === -1 && i.name !== "lo";
      })
      .slice(0, 4)
      .forEach(function (i, idx) {
        detailSet("detail-net", "nic" + idx, i.name.slice(0, 18),
          "↓" + fmtRate(i.download_bytes_per_sec) + " ↑" + fmtRate(i.upload_bytes_per_sec), "dim");
      });
  }

  function setDetail(open) {
    detailOpen = open;
    document.getElementById("detail").hidden = !open;
    if (open && lastSnapshot) renderDetail(lastSnapshot.host || {});
  }

  /* ------------------------------------------------------------- transport */

  function showBanner(title, text, info) {
    document.getElementById("banner-title").textContent = title;
    document.getElementById("banner-text").textContent = text;
    var banner = document.getElementById("banner");
    banner.className = "banner" + (info ? " info" : "");
    banner.hidden = false;
  }

  function hideBanner() {
    document.getElementById("banner").hidden = true;
  }

  /** Report why the API could not be read, in terms that name the fix. */
  function noteApiFailure(path, status, immediate) {
    apiFailures += 1;
    // One miss can be a restart; a 404 needs no second opinion.
    if (apiFailures < 2 && !immediate && status !== 404) return;
    var overall = document.getElementById("overall");
    overall.textContent = "NO DATA";
    overall.className = "overall critical";
    var origin = window.location.origin;
    if (status === 404) {
      showBanner("WRONG SERVER ON THIS PORT", origin + path +
        " returned 404. Something other than PLH Rack Monitor is serving this port.");
    } else if (status) {
      showBanner("BACKEND ERROR", origin + path + " returned HTTP " + status + ".");
    } else {
      showBanner("BACKEND UNREACHABLE", "No response from " + origin + path +
        ". Start it with: plh-rack-monitor run");
    }
  }

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
    if (document.getElementById("banner").className.indexOf("info") === -1) hideBanner();
    renderHost(snapshot.host || {}, snapshot.service);
    (snapshot.nodes || []).forEach(renderNode);
    renderOverall(snapshot);
    if (detailOpen) renderDetail(snapshot.host || {});
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
      getJson("/api/metrics").then(applySnapshot).catch(function (error) {
        setLink("down", "NO DATA");
        noteApiFailure("/api/metrics", error.status);
      });
    }, 2000);
  }

  function stopPolling() {
    if (pollTimer) clearInterval(pollTimer);
    pollTimer = null;
  }

  function connectStream() {
    if (!window.EventSource) {
      startPolling();
      return;
    }
    var source;
    try {
      source = new EventSource("/api/stream");
    } catch (e) {
      startPolling();
      return;
    }
    source.addEventListener("open", function () {
      stopPolling();
      setLink("live", "LIVE");
    });
    source.addEventListener("metrics", function (event) {
      try {
        applySnapshot(JSON.parse(event.data));
        stopPolling();
        setLink("live", "LIVE");
      } catch (e) {
        /* A malformed frame is skipped; the next one is a full snapshot. */
      }
    });
    source.addEventListener("error", function () {
      // EventSource retries on its own; polling covers the gap meanwhile.
      setLink("polling", "RECONNECTING");
      startPolling();
    });
  }

  function showWarnings(list) {
    var badge = document.getElementById("warnings");
    if (!list || !list.length) {
      badge.hidden = true;
      return;
    }
    badge.hidden = false;
    badge.textContent = "! " + list.length;
    badge.title = "Configuration warnings:\n" + list.join("\n");
    badge.onclick = function (event) {
      event.stopPropagation();
      showBanner("CONFIG", list.length + " warning(s): " + list.join(" | "), true);
      setTimeout(hideBanner, 12000);
    };
  }

  /* ------------------------------------------------------------ fullscreen */

  function toggleFullscreen() {
    if (document.fullscreenElement) {
      if (document.exitFullscreen) document.exitFullscreen();
    } else if (document.documentElement.requestFullscreen) {
      document.documentElement.requestFullscreen().catch(function () {});
    }
  }

  function onKey(event) {
    if (event.key === "F11") {
      // Native F11 is not blocked. If the browser handled it the viewport
      // changes and nothing more is done; otherwise the API is used.
      var was = !!document.fullscreenElement;
      var height = window.innerHeight;
      setTimeout(function () {
        if (!!document.fullscreenElement === was && window.innerHeight === height) toggleFullscreen();
      }, 300);
      return;
    }
    if (event.key === "d" || event.key === "D") setDetail(!detailOpen);
    else if (event.key === "Escape" && detailOpen) setDetail(false);
    else if (event.key === "0" || event.key === "a" || event.key === "A") applyView("all");
    else if (/^[1-9]$/.test(event.key)) {
      var key = order[Number(event.key) - 1];
      if (key) applyView(key);
    } else if (event.key === "ArrowRight" || event.key === "PageDown") turnPage(1);
    else if (event.key === "ArrowLeft" || event.key === "PageUp") turnPage(-1);
  }

  /* ------------------------------------------------------------------ init */

  function start(cfgLoaded) {
    document.title = config.display.title || "PLH Rack Monitor";
    document.getElementById("brand-name").textContent = config.display.title || "PLH RACK MONITOR";
    buildPanels();
    showWarnings(config.config_warnings);

    var stored = null;
    try {
      stored = localStorage.getItem(VIEW_KEY);
    } catch (e) {
      stored = null;
    }
    applyView(stored || "all");

    if (!cfgLoaded) return;
    connectStream();
    getJson("/api/metrics").then(applySnapshot).catch(function (error) {
      noteApiFailure("/api/metrics", error.status);
    });
  }

  function init() {
    var resizeQueued = false;
    window.addEventListener("resize", function () {
      if (resizeQueued) return;
      resizeQueued = true;
      requestAnimationFrame(function () {
        resizeQueued = false;
        layout();
      });
    });
    document.addEventListener("keydown", onKey);
    document.getElementById("fs-btn").addEventListener("click", toggleFullscreen);
    document.getElementById("detail-btn").addEventListener("click", function (event) {
      event.stopPropagation();
      setDetail(!detailOpen);
    });

    getJson("/api/config")
      .then(function (cfg) {
        if (cfg.thresholds) config.thresholds = cfg.thresholds;
        if (cfg.display) config.display = cfg.display;
        if (cfg.host) config.host = cfg.host;
        if (Array.isArray(cfg.nodes)) config.nodes = cfg.nodes;
        config.config_warnings = cfg.config_warnings || [];
        start(true);
      })
      .catch(function (error) {
        // Defaults keep the page usable; a 404 here is the clearest sign of
        // the wrong server on this port.
        start(false);
        noteApiFailure("/api/config", error.status, true);
        startPolling();
      });

    // A stream that stops delivering is surfaced rather than left looking live.
    setInterval(function () {
      if (lastFrameAt && Date.now() - lastFrameAt > 8000) setLink("down", "STALLED");
    }, 2000);
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();

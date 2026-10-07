// The relay's stats page (/stats): asks /api/v1/stats with the key saved in
// this browser and draws it. Everything goes in with textContent, never HTML.

const KEY = "yacs-stats-key";
const REFRESH_MS = 60_000;
const $ = (id) => document.getElementById(id);

// Storage may be off (private mode); the key then lasts until the page closes.
let memoryKey = null;
function savedKey() {
  try {
    return localStorage.getItem(KEY);
  } catch {
    return memoryKey;
  }
}
function saveKey(key) {
  memoryKey = key;
  try {
    if (key === null) localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, key);
  } catch {}
}

const count = new Intl.NumberFormat();
function bytes(n) {
  const units = ["B", "kB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1000 && i < units.length - 1) {
    n /= 1000;
    i++;
  }
  return `${i === 0 ? n : n.toFixed(n < 10 ? 1 : 0)} ${units[i]}`;
}
function duration(secs) {
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  return d > 0 ? `${d} d ${h} h` : h > 0 ? `${h} h ${m} min` : `${m} min`;
}
const refused = (u) => u.limited + u.too_large + u.storage_full + u.unauthorized + u.errors;

// What the charts can show; `hours: false` for what only makes sense per day.
const METRICS = [
  { name: "Traffic", value: (u) => u.bytes_in + u.bytes_out, format: bytes },
  { name: "Requests", value: (u) => u.requests, format: count.format },
  { name: "Clips", value: (u) => u.clips, format: count.format },
  { name: "Spaces used", value: (u) => u.active_spaces, format: count.format, hours: false },
  { name: "New spaces", value: (u) => u.new_spaces, format: count.format },
  { name: "Refused", value: refused, format: count.format },
];

let stats = null;

async function load() {
  const key = savedKey();
  if (!key) return showLogin();
  let res;
  try {
    res = await fetch("/api/v1/stats", { headers: { authorization: `Bearer ${key}` }, cache: "no-store" });
  } catch {
    return showError("Can't reach the relay. Trying again in a minute.");
  }
  if (res.status === 401) {
    saveKey(null);
    return showLogin("That key isn't the relay's stats key or account key.");
  }
  if (res.status === 404) {
    return showError("This relay has no stats: it needs version 0.7.4 or later, and YACS_STATS_TOKEN or YACS_ACCESS_TOKEN set.");
  }
  if (!res.ok) return showError(`The relay answered ${res.status}. Trying again in a minute.`);
  stats = await res.json();
  showError(null);
  render();
}

function showLogin(message = null) {
  $("login").hidden = false;
  $("stats").hidden = true;
  $("refresh").hidden = true;
  showError(message);
  $("key").focus();
}

function showError(message) {
  $("error").hidden = message === null;
  $("error").textContent = message ?? "";
}

function render() {
  $("login").hidden = true;
  $("stats").hidden = false;
  $("refresh").hidden = false;
  $("about").textContent = `${stats.version} · up ${duration(stats.uptime_secs)}`;
  $("updated").textContent = `as of ${new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;

  const share = stats.disk_max_bytes ? stats.disk_used_bytes / stats.disk_max_bytes : 0;
  $("disk").textContent = `${(share * 100).toFixed(share < 0.1 ? 1 : 0)} %`;
  $("disk-sub").textContent = `${bytes(stats.disk_used_bytes)} of ${bytes(stats.disk_max_bytes)}, free spaces up to ${bytes(stats.free_disk_max_bytes)}`;
  $("disk-meter").firstElementChild.style.width = `${Math.min(share, 1) * 100}%`;
  $("disk-meter").classList.toggle("high", share >= 0.8);

  const s = stats.spaces;
  $("spaces").textContent = s ? count.format(s.owner + s.free) : "–";
  $("spaces-sub").textContent = s ? `${count.format(s.free)} free, ${count.format(s.owner)} yours` : "this relay doesn't register spaces";
  $("active").textContent = s ? count.format(s.active_today) : "–";
  $("active-sub").textContent = s ? `today; ${count.format(s.active_week)} this week` : "";
  $("listeners").textContent = count.format(stats.listeners);

  renderRecent();
  renderChart($("days"), stats.days, "day");
  renderChart($("hours"), stats.hours, "hour");
}

function renderRecent() {
  const h = stats.last_hour;
  const t = stats.today;
  const rows = [
    ["Requests", h.requests, t.requests, count.format],
    ["Data in", h.bytes_in, t.bytes_in, bytes],
    ["Data out", h.bytes_out, t.bytes_out, bytes],
    ["Clips", h.clips, t.clips, count.format],
    ["New spaces", h.new_spaces, t.new_spaces, count.format],
    ["Spaces used", null, t.active_spaces, count.format],
    ["Rate limits and quotas", h.limited, t.limited, count.format],
    ["Too large", h.too_large, t.too_large, count.format],
    ["Wrong account key", h.unauthorized, t.unauthorized, count.format],
    ["Disk full", h.storage_full, t.storage_full, count.format, true],
    ["Errors", h.errors, t.errors, count.format, true],
  ];
  const table = $("recent");
  table.replaceChildren();
  const head = table.insertRow();
  for (const label of ["", "Last hour", "Today"]) {
    const th = document.createElement("th");
    th.textContent = label;
    head.append(th);
  }
  for (const [label, hour, today, format, bad] of rows) {
    const row = table.insertRow();
    row.insertCell().textContent = label;
    for (const value of [hour, today]) {
      const cell = row.insertCell();
      cell.textContent = value === null ? "–" : format(value);
      if (bad && value > 0) cell.className = "bad";
    }
  }
}

const chartMetric = { day: METRICS[0], hour: METRICS[0] };
const SVG = "http://www.w3.org/2000/svg";

function renderChart(box, buckets, unit) {
  const metrics = METRICS.filter((m) => unit === "day" || m.hours !== false);
  const metric = chartMetric[unit];
  box.replaceChildren();

  const head = document.createElement("div");
  head.className = "chart-head";
  for (const m of metrics) {
    const button = document.createElement("button");
    button.textContent = m.name;
    button.setAttribute("aria-pressed", String(m === metric));
    button.onclick = () => {
      chartMetric[unit] = m;
      renderChart(box, buckets, unit);
    };
    head.append(button);
  }

  const values = buckets.map((b) => metric.value(b));
  const max = Math.max(...values, 1);
  const total = values.reduce((a, b) => a + b, 0);
  const label = (b) => (unit === "day" ? b.start.slice(0, 10) : `${b.start.slice(0, 10)} ${b.start.slice(11, 16)}`);

  const readout = document.createElement("div");
  readout.className = "readout muted";
  const summary = `${unit === "day" ? `${buckets.length} days` : "48 hours"}: ${metric.format(total)} in all, at most ${metric.format(Math.max(...values, 0))} per ${unit}`;
  readout.textContent = summary;

  const width = 1000;
  const height = 160;
  const svg = document.createElementNS(SVG, "svg");
  svg.setAttribute("viewBox", `0 0 ${width} ${height}`);
  svg.setAttribute("preserveAspectRatio", "none");
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", `${metric.name} per ${unit}`);
  const line = document.createElementNS(SVG, "line");
  line.setAttribute("class", "grid");
  line.setAttribute("x1", "0");
  line.setAttribute("x2", String(width));
  line.setAttribute("y1", String(height - 0.5));
  line.setAttribute("y2", String(height - 0.5));
  svg.append(line);
  const step = width / buckets.length;
  buckets.forEach((b, i) => {
    const value = values[i];
    const h = value === 0 ? 0 : Math.max((value / max) * (height - 4), 2);
    const bar = document.createElementNS(SVG, "rect");
    bar.setAttribute("x", String(i * step + step * 0.1));
    bar.setAttribute("width", String(step * 0.8));
    // Full height, so a quiet bucket can be pointed at too; only the fill shows the value.
    bar.setAttribute("y", "0");
    bar.setAttribute("height", String(height));
    bar.style.fill = "transparent";
    const fill = document.createElementNS(SVG, "rect");
    fill.setAttribute("class", "bar");
    fill.setAttribute("x", String(i * step + step * 0.1));
    fill.setAttribute("width", String(step * 0.8));
    fill.setAttribute("y", String(height - h));
    fill.setAttribute("height", String(h));
    fill.style.pointerEvents = "none";
    const show = () => (readout.textContent = `${label(b)}: ${metric.format(value)}`);
    bar.addEventListener("pointerenter", show);
    bar.addEventListener("click", show);
    bar.addEventListener("pointerleave", () => (readout.textContent = summary));
    svg.append(bar, fill);
  });

  const axis = document.createElement("div");
  axis.className = "muted";
  axis.style.cssText = "display:flex;justify-content:space-between;font-size:12px;margin-top:4px";
  const first = document.createElement("span");
  first.textContent = label(buckets[0]);
  const last = document.createElement("span");
  last.textContent = unit === "day" ? "today" : "now";
  axis.append(first, last);

  box.append(head, svg, axis, readout);
}

$("key-form").addEventListener("submit", (event) => {
  event.preventDefault();
  saveKey($("key").value.trim());
  $("key").value = "";
  load();
});
$("refresh").addEventListener("click", load);
$("forget").addEventListener("click", () => {
  saveKey(null);
  stats = null;
  showLogin();
});
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible" && stats) load();
});
setInterval(() => {
  if (document.visibilityState === "visible" && savedKey()) load();
}, REFRESH_MS);
load();

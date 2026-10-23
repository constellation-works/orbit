// Orbit dashboard Analytics › Resources: what one delivery of each kind costs
// on this host, minute by minute since the drain admitted it. These are the
// resource profiles admission forecasts with (`orbit run profiles`), read from
// /api/resource-profiles for the selected workspace.
// Pure vanilla JS, split into ES modules with no build step.
//
// Profiles are per host: the endpoint answers for the host it runs on, and a
// remote host's profiles come through its forward. The aggregate workspace
// view shows the usual placeholder rather than merging workspaces.
//
// View state (work class, units, hidden series, hover minute) is local and
// re-renders from the cached payload without a fetch. CPU, memory and the
// still-running strip share one x-axis and one hover cursor.

import {
  el as makeEl, syncNodes, fetchJson, requestPanel, panelCanRender, getWorkspaceRevision,
  onWorkspaceChange, renderPanelPlaceholder, isAggregateView, formatDateTime,
} from './common.js';

const $ = (id) => document.getElementById(id);

// `el` from common.js, also applying the table and focus attributes it leaves out.
function el(tag, opts = {}, children = []) {
  const node = makeEl(tag, opts, children);
  for (const name of ["scope", "tabindex"]) {
    if (opts[name] != null) node.setAttribute(name, String(opts[name]));
  }
  return node;
}

const SVG_NS = "http://www.w3.org/2000/svg";
const GIB = 1024 ** 3;
const BODY = "resources-body";
const ENDPOINT = "/api/resource-profiles";

// Complexity series in overlay order. Colours match Analytics › Metrics for
// the complexities both views show.
const COMPLEXITIES = [
  { key: "low", label: "Low", tag: "low" },
  { key: "medium", label: "Medium", tag: "medium" },
  { key: "hard", label: "Hard", tag: "hard" },
  { key: "xhard", label: "Xhard", tag: "xhard" },
  { key: "unassessed", label: "Unassessed", tag: "unassessed" },
  { key: "unset", label: "Unset", tag: "no complexity" },
];
const BASE_CLASSES = [
  ["change", "Change"],
  ["no-diff", "No-diff"],
];
const TEMPLATE_PREFIX = "auto-task:";
const UNITS = [["abs", "Cores · GiB"], ["pct", "% of host"]];

const view = {
  workClass: null,
  unit: "abs",
  hidden: {},
  hover: null,
};
let lastPayload = null;
let requestScope = null;

function clearResources() {
  lastPayload = null;
  view.hover = null;
  const count = $("resources-count");
  if (count) count.textContent = "—";
}

export function placeholdResourcesAggregate() {
  clearResources();
  renderPanelPlaceholder(BODY);
}

export function fetchAndRenderResources() {
  if (isAggregateView()) {
    placeholdResourcesAggregate();
    return Promise.resolve();
  }
  const revision = getWorkspaceRevision();
  if (requestScope !== `${revision}`) {
    requestScope = `${revision}`;
    clearResources();
  }
  return requestPanel(BODY, ENDPOINT, () => fetchJson(ENDPOINT), (payload) => {
    if (getWorkspaceRevision() !== revision) return;
    lastPayload = payload;
    renderResources();
  }, "resources-count");
}

onWorkspaceChange(() => {
  requestScope = null;
  clearResources();
});

export function renderResources() {
  const body = $(BODY);
  if (!body) return;
  if (isAggregateView()) {
    placeholdResourcesAggregate();
    return;
  }
  if (!lastPayload || !panelCanRender(BODY)) return;
  const profiles = lastPayload.profiles || [];
  const count = $("resources-count");
  if (count) count.textContent = `${profiles.length} profile${profiles.length === 1 ? "" : "s"}${hostText(lastPayload.host) ? ` · ${hostText(lastPayload.host)}` : ""}`;
  const node = profiles.length ? profileSection() : emptySection();
  node.dataset.key = "resources-view";
  node.dataset.hash = JSON.stringify([view.workClass, view.unit, view.hidden, lastPayload]);
  syncNodes(body, [node]);
}

function rerender() {
  renderResources();
}

// ---- formatting -----------------------------------------------------------

function hostText(host) {
  if (!host) return "";
  return `host ${host.cpu_count} cores · ${fmtGib(host.memory_total_bytes)}`;
}

function fmtGib(bytes) {
  const gib = Number(bytes) / GIB;
  if (!Number.isFinite(gib)) return "—";
  return `${gib >= 10 ? Math.round(gib) : gib.toFixed(1)} GiB`;
}

function fmtCores(cores) {
  const value = Number(cores);
  if (!Number.isFinite(value)) return "—";
  return `${value >= 10 ? value.toFixed(0) : value.toFixed(1)} cores`;
}

function fmtPct(value) {
  if (!Number.isFinite(value)) return "—";
  return `${value >= 10 ? Math.round(value) : value.toFixed(1)}%`;
}

function minuteLabel(minutes) {
  if (minutes === 0) return "0";
  if (minutes < 60) return `${minutes}m`;
  if (minutes % 60 === 0) return `${minutes / 60}h`;
  return `${Math.floor(minutes / 60)}h${String(minutes % 60).padStart(2, "0")}`;
}

/// Gridline steps for `max`: about four ticks on a 1-2-2.5-5 ladder.
function niceTicks(max, target = 4) {
  const span = max > 0 ? max : 1;
  const raw = span / target;
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * magnitude).find((s) => s >= raw) || raw;
  const top = Math.ceil(span / step - 1e-9) * step;
  const ticks = [];
  for (let v = 0; v <= top + step / 1e6; v += step) ticks.push(Number(v.toPrecision(6)));
  return { top, ticks };
}

/// Minute ticks on round steps, about six across.
function minuteTicks(maxMinute) {
  const step = [1, 2, 5, 10, 15, 30, 60, 120, 240].find((s) => maxMinute / s <= 7) || 480;
  const ticks = [];
  for (let m = 0; m <= maxMinute; m += step) ticks.push(m);
  return ticks;
}

function svg(tag, attrs = {}) {
  const node = document.createElementNS(SVG_NS, tag);
  for (const [name, value] of Object.entries(attrs)) node.setAttribute(name, String(value));
  return node;
}

// ---- model ----------------------------------------------------------------

function classLabel(workClass) {
  const base = BASE_CLASSES.find(([key]) => key === workClass);
  if (base) return base[1];
  return workClass.startsWith(TEMPLATE_PREFIX) ? workClass.slice(TEMPLATE_PREFIX.length) : workClass;
}

/// change and no-diff always, then every auto-task template with a profile.
function workClasses(profiles) {
  const present = new Set(profiles.map((profile) => profile.key.work_class));
  const templates = [...present].filter((name) => !BASE_CLASSES.some(([key]) => key === name)).sort();
  return [...BASE_CLASSES.map(([key]) => key), ...templates].map((key) => ({ key, label: classLabel(key), available: present.has(key) }));
}

function selectedClass(classes) {
  const current = classes.find((entry) => entry.key === view.workClass && entry.available);
  if (current) return current.key;
  const first = classes.find((entry) => entry.available);
  return first ? first.key : classes[0].key;
}

/// One series per profile of `workClass`: complexities in overlay order, or
/// the template's single profile. Each ends at its horizon.
function seriesFor(workClass) {
  const profiles = (lastPayload.profiles || []).filter((profile) => profile.key.work_class === workClass);
  const isTemplate = workClass.startsWith(TEMPLATE_PREFIX);
  const series = profiles.map((profile) => {
    const complexity = profile.key.complexity == null ? "unset" : profile.key.complexity;
    const known = COMPLEXITIES.find((entry) => entry.key === complexity);
    const key = isTemplate ? "template" : (known ? known.key : "unset");
    const horizon = Number(profile.horizon_minutes) || 0;
    return {
      key,
      label: isTemplate ? classLabel(workClass) : (known ? known.label : complexity),
      tag: isTemplate ? "template" : (known ? known.tag : complexity),
      order: isTemplate ? 0 : COMPLEXITIES.findIndex((entry) => entry.key === key),
      profile,
      horizon,
      sparse: !!profile.sparse,
      deliveries: profile.delivery_count,
      minutes: (profile.series || []).filter((minute) => minute.minute <= horizon),
    };
  });
  return series.sort((a, b) => a.order - b.order);
}

function percentAvailable() {
  const host = lastPayload.host;
  return !!(host && host.cpu_count > 0 && host.memory_total_bytes > 0);
}

/// Value transforms for the unit in view.
function units() {
  const pct = view.unit === "pct" && percentAvailable();
  const host = lastPayload.host;
  return {
    pct,
    cpu: (cores) => (pct ? cores / host.cpu_count * 100 : cores),
    mem: (bytes) => (pct ? bytes / host.memory_total_bytes * 100 : bytes / GIB),
    cpuText: (cores) => (pct ? fmtPct(cores / host.cpu_count * 100) : fmtCores(cores)),
    memText: (bytes) => (pct ? fmtPct(bytes / host.memory_total_bytes * 100) : fmtGib(bytes)),
    axis: (v) => (pct ? `${v}%` : `${v}`),
  };
}

// ---- sections -------------------------------------------------------------

function emptySection() {
  const p = lastPayload;
  return el("section", { class: "mx-card rp-card", "aria-label": "Resource profiles" }, [
    el("div", { class: "mx-empty" }, [
      el("div", { class: "mx-empty-title", text: "No resource profiles on this host for this workspace yet" }),
      el("div", { class: "mx-empty-detail", text: `A profile is learned from the CPU and memory each delivery's workers recorded on this host. It appears once a delivery the drain admitted here finishes; each covers the last ${p.profile_window} completed deliveries of its kind within 30 days.` }),
    ]),
    metaLine(),
  ]);
}

function profileSection() {
  const classes = workClasses(lastPayload.profiles || []);
  const workClass = selectedClass(classes);
  view.workClass = workClass;
  const all = seriesFor(workClass);
  const hidden = new Set(view.hidden[workClass] || []);
  const shown = all.filter((series) => !hidden.has(series.key));
  const visible = shown.length ? shown : all;
  const u = units();
  return el("section", { class: "mx-card rp-card", "aria-label": "Resource profiles" }, [
    el("div", { class: "rp-head" }, [
      classPicker(classes, workClass),
      unitPicker(),
    ]),
    el("p", { class: "mx-blurb", text: "What one delivery costs on this host, minute by minute since the drain admitted it: the curves admission forecasts with. Each minute averages the deliveries still running then, so the tail reflects only long runs." }),
    seriesCards(all, hidden, workClass, u),
    charts(visible, u),
    profileTable(all, u),
    metaLine(),
  ]);
}

function classPicker(classes, current) {
  return el("div", { class: "mx-seg rp-classes", role: "group", "aria-label": "Work class" }, classes.map((entry) => {
    const on = entry.key === current;
    const button = el("button", {
      class: on ? "mx-seg-btn on" : "mx-seg-btn",
      type: "button",
      text: entry.label,
      "aria-pressed": on ? "true" : "false",
      title: entry.available ? (entry.key.startsWith(TEMPLATE_PREFIX) ? `Auto-task template ${entry.label}` : `${entry.label} deliveries`) : `No ${entry.label.toLowerCase()} profiles on this host yet`,
    });
    button.dataset.workClass = entry.key;
    button.disabled = !entry.available;
    button.addEventListener("click", () => {
      if (view.workClass === entry.key) return;
      view.workClass = entry.key;
      view.hover = null;
      rerender();
      focusIn(`.rp-classes [data-work-class="${CSS.escape(entry.key)}"]`);
    });
    return button;
  }));
}

function unitPicker() {
  const available = percentAvailable();
  const current = view.unit === "pct" && available ? "pct" : "abs";
  return el("div", { class: "mx-seg", role: "group", "aria-label": "Units" }, UNITS.map(([key, text]) => {
    const on = key === current;
    const button = el("button", {
      class: on ? "mx-seg-btn on" : "mx-seg-btn",
      type: "button",
      text,
      "aria-pressed": on ? "true" : "false",
      title: key === "pct" && !available ? "This host's size could not be read" : null,
    });
    button.dataset.unit = key;
    button.disabled = key === "pct" && !available;
    button.addEventListener("click", () => {
      if (view.unit === key) return;
      view.unit = key;
      rerender();
      focusIn(`[data-unit="${key}"]`);
    });
    return button;
  }));
}

function focusIn(selector) {
  const node = $(BODY) && $(BODY).querySelector(selector);
  if (node) node.focus();
}

function seriesCards(all, hidden, workClass, u) {
  const min = lastPayload.min_profile_deliveries;
  return el("div", { class: "mx-series", role: "group", "aria-label": "Series" }, all.map((series) => {
    const on = !hidden.has(series.key);
    const peakCpu = series.profile.peak_cpu_cores || {};
    const peakMem = series.profile.peak_memory_bytes || {};
    const button = el("button", {
      class: `mx-sc rp-s-${series.key}${on ? " on" : " off"}${series.sparse ? " rp-sparse" : ""}`,
      type: "button",
      "aria-pressed": on ? "true" : "false",
      title: `${on ? "Hide" : "Show"} ${series.label}${series.sparse ? ` (sparse: fewer than ${min} deliveries)` : ""}`,
    }, [
      el("span", { class: "mx-sc-label" }, [
        el("span", { class: "mx-swatch", "aria-hidden": "true" }),
        series.label,
        series.tag && series.tag !== series.label.toLowerCase() ? el("span", { class: "mx-sc-tag", text: series.tag }) : null,
      ]),
      el("span", { class: "mx-sc-value", text: u.cpuText(peakCpu.value) }),
      el("span", { class: "mx-sc-delta" }, [
        el("span", { class: "mx-sc-sub", text: `peak at +${peakCpu.minute ?? 0}m · mem p90 ${u.memText(peakMem.value)}` }),
      ]),
      el("span", { class: "mx-sc-foot" }, [
        el("span", { class: series.sparse ? "rp-n low" : "rp-n", text: `${series.deliveries} deliveries` }),
        series.sparse ? el("span", { class: "rp-sparse-tag", text: "sparse" }) : null,
        el("span", { text: ` · to +${series.horizon}m` }),
      ]),
    ]);
    button.addEventListener("click", () => {
      const next = new Set(hidden);
      if (next.has(series.key)) next.delete(series.key);
      else next.add(series.key);
      // The charts always keep one series.
      if (next.size >= all.length) return;
      view.hidden[workClass] = [...next];
      rerender();
    });
    return button;
  }));
}

// ---- charts ---------------------------------------------------------------

function charts(visible, u) {
  const maxMinute = Math.max(1, ...visible.map((series) => series.horizon));
  const x = (minute) => minute / maxMinute * 100;

  let cpuMax = 0;
  let memMax = 0;
  for (const series of visible) {
    for (const m of series.minutes) {
      cpuMax = Math.max(cpuMax, u.cpu(m.cpu_cores + m.cpu_stddev));
      memMax = Math.max(memMax, u.mem(m.memory_bytes), u.mem(m.memory_mean_bytes));
    }
  }
  const cpuThreshold = u.pct ? lastPayload.admit_cpu_percent : null;
  const memThreshold = u.pct ? lastPayload.admit_memory_percent : null;
  if (cpuThreshold != null) cpuMax = Math.max(cpuMax, cpuThreshold);
  if (memThreshold != null) memMax = Math.max(memMax, memThreshold);
  const cpuScale = niceTicks(cpuMax * 1.05);
  const memScale = niceTicks(memMax * 1.05);

  const cpuPlot = plot({
    kind: "cpu",
    title: u.pct ? "CPU · mean % of host cores, ±1 sd band" : "CPU · mean cores, ±1 sd band",
    scale: cpuScale,
    axis: u.axis,
    threshold: cpuThreshold,
    thresholdLabel: `admit ${cpuThreshold}% of cores`,
    visible, x,
    draw: (group, series, y) => {
      const pts = series.minutes;
      if (!pts.length) return;
      const upper = pts.map((m) => `${x(m.minute) * 10} ${y(u.cpu(m.cpu_cores + m.cpu_stddev))}`);
      const lower = pts.map((m) => `${x(m.minute) * 10} ${y(u.cpu(Math.max(0, m.cpu_cores - m.cpu_stddev)))}`).reverse();
      group.appendChild(svg("path", { d: `M${upper.join(" L")} L${lower.join(" L")} Z`, class: "rp-band" }));
      group.appendChild(svg("path", { d: line(pts.map((m) => [x(m.minute), y(u.cpu(m.cpu_cores))])), class: "rp-line", "vector-effect": "non-scaling-stroke" }));
    },
    end: (series) => (series.minutes.length ? u.cpu(series.minutes[series.minutes.length - 1].cpu_cores) : null),
  });
  const memPlot = plot({
    kind: "mem",
    title: u.pct ? "Memory · p90 % of host memory (solid), mean (dashed)" : "Memory · p90 GiB (solid), mean (dashed)",
    scale: memScale,
    axis: u.axis,
    threshold: memThreshold,
    thresholdLabel: `admit ${memThreshold}% of memory`,
    visible, x,
    draw: (group, series, y) => {
      const pts = series.minutes;
      if (!pts.length) return;
      group.appendChild(svg("path", { d: line(pts.map((m) => [x(m.minute), y(u.mem(m.memory_mean_bytes))])), class: "rp-line rp-mean", "vector-effect": "non-scaling-stroke" }));
      group.appendChild(svg("path", { d: line(pts.map((m) => [x(m.minute), y(u.mem(m.memory_bytes))])), class: "rp-line", "vector-effect": "non-scaling-stroke" }));
    },
    end: (series) => (series.minutes.length ? u.mem(series.minutes[series.minutes.length - 1].memory_bytes) : null),
  });
  const alivePlot = plot({
    kind: "alive",
    title: "Still running · share of deliveries",
    scale: { top: 100, ticks: [0, 100] },
    axis: (v) => `${v}%`,
    visible, x,
    draw: (group, series, y) => {
      const pts = series.minutes;
      if (!pts.length) return;
      group.appendChild(svg("path", { d: line(pts.map((m) => [x(m.minute), y(m.alive * 100)])), class: "rp-line", "vector-effect": "non-scaling-stroke" }));
    },
    end: (series) => (series.minutes.length ? series.minutes[series.minutes.length - 1].alive * 100 : null),
  });

  const xAxis = el("div", { class: "rp-x-axis", "aria-hidden": "true" }, minuteTicks(maxMinute).map((minute) => {
    const tick = el("div", { class: "mx-x-tick", text: minuteLabel(minute) });
    tick.style.left = `${x(minute)}%`;
    return tick;
  }));
  const plots = [cpuPlot, memPlot, alivePlot];
  const tip = el("div", { class: "mx-tip rp-tip", "aria-hidden": "true" });
  tip.hidden = true;
  const live = el("div", { class: "visually-hidden", role: "status", "aria-live": "polite" });
  const stack = el("div", { class: "rp-stack" }, [...plots.map((entry) => entry.node), el("div", { class: "rp-x-row" }, [el("div", { class: "rp-y-gutter", "aria-hidden": "true" }), xAxis]), tip]);
  const frame = el("div", {
    class: "rp-frame",
    tabindex: "0",
    role: "group",
    "aria-label": `${visible.map((series) => series.label).join(", ")} resource profiles: arrow keys step through minutes since admission, Shift with an arrow by ten`,
  }, [stack, live]);

  const show = (minute) => {
    view.hover = minute;
    for (const entry of plots) entry.cursor(minute == null ? null : x(minute));
    if (minute == null) {
      tip.hidden = true;
      return;
    }
    fillTip(tip, minute, visible, u);
    tip.hidden = false;
    tip.style.left = `calc(var(--rp-gutter) + (100% - var(--rp-gutter)) * ${x(minute) / 100})`;
    tip.classList.toggle("left", x(minute) > 55);
    live.textContent = tipText(minute, visible, u);
  };
  const minuteAt = (event) => {
    const area = cpuPlot.area.getBoundingClientRect();
    if (!area.width) return null;
    const ratio = (event.clientX - area.left) / area.width;
    if (ratio < 0 || ratio > 1) return null;
    return Math.round(ratio * maxMinute);
  };
  stack.addEventListener("mousemove", (event) => {
    const minute = minuteAt(event);
    if (minute !== view.hover) show(minute);
  });
  stack.addEventListener("mouseleave", () => show(null));
  frame.addEventListener("keydown", (event) => {
    if (event.key === "Escape") { show(null); return; }
    const steps = { ArrowRight: 1, ArrowLeft: -1 };
    let next = null;
    if (event.key in steps) {
      const from = view.hover == null ? (steps[event.key] > 0 ? -1 : maxMinute + 1) : view.hover;
      next = from + steps[event.key] * (event.shiftKey ? 10 : 1);
    } else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = maxMinute;
    if (next == null) return;
    event.preventDefault();
    show(Math.max(0, Math.min(maxMinute, next)));
  });
  frame.addEventListener("blur", () => show(null));
  if (view.hover != null && view.hover <= maxMinute) queueMicrotask(() => show(view.hover));

  const min = lastPayload.min_profile_deliveries;
  const notes = el("div", { class: "mx-notes rp-notes" }, [
    el("span", { class: "mx-note-hollow" }, [el("span", { class: "mx-pt-key", "aria-hidden": "true" }), `dimmed, hollow end = sparse: fewer than ${min} deliveries, so admission falls back to a wider profile`]),
    el("span", { text: "each line ends at its horizon, where fewer than 1 in 10 deliveries still run" }),
    u.pct ? el("span", { text: "dashed rule = admit threshold, which bounds the whole host's forecast, not one delivery" }) : null,
    el("span", { text: "x = minutes since admission" }),
  ]);
  return el("div", { class: "rp-charts" }, [frame, notes]);
}

/// One plot of the stack: its title, y axis, gridlines, lines and cursor.
function plot({ kind, title, scale, axis, threshold, thresholdLabel, visible, x, draw, end }) {
  const y = (value) => 1000 - Math.min(Math.max(value, 0), scale.top) / scale.top * 1000;
  const top = (value) => y(value) / 10;
  const yAxis = el("div", { class: "rp-y-axis", "aria-hidden": "true" }, scale.ticks.map((tick) => {
    const label = el("div", { class: "mx-y-tick", text: axis(tick) });
    label.style.top = `${top(tick)}%`;
    return label;
  }));
  const area = el("div", { class: `rp-area rp-area-${kind}` });
  for (const tick of scale.ticks) {
    const grid = el("div", { class: "mx-grid" });
    grid.style.top = `${top(tick)}%`;
    area.appendChild(grid);
  }
  if (threshold != null) {
    const rule = el("div", { class: "rp-threshold" });
    rule.style.top = `${top(threshold)}%`;
    const label = el("div", { class: "rp-threshold-label", text: thresholdLabel });
    label.style.top = `${top(threshold)}%`;
    area.append(rule, label);
  }
  const lines = svg("svg", { viewBox: "0 0 1000 1000", preserveAspectRatio: "none", class: "rp-lines", "aria-hidden": "true" });
  for (const series of visible) {
    const group = svg("g", { class: `rp-s-${series.key}${series.sparse ? " rp-sparse" : ""}` });
    draw(group, series, y);
    lines.appendChild(group);
  }
  area.appendChild(lines);
  for (const series of visible) {
    const value = end(series);
    if (value == null) continue;
    const dot = el("div", { class: `mx-pt rp-end rp-s-${series.key}${series.sparse ? " hollow rp-sparse" : ""}`, "aria-hidden": "true" });
    // The last minute drawn: the horizon, or earlier when every delivery had
    // finished by then.
    dot.style.left = `${x(series.minutes[series.minutes.length - 1].minute)}%`;
    dot.style.top = `${top(value)}%`;
    area.appendChild(dot);
  }
  const cursor = el("div", { class: "mx-cursor", "aria-hidden": "true" });
  cursor.hidden = true;
  area.appendChild(cursor);
  const node = el("div", { class: `rp-plot rp-plot-${kind}` }, [
    el("div", { class: "rp-plot-title", text: title }),
    el("div", { class: "rp-plot-body" }, [yAxis, area]),
  ]);
  return {
    node,
    area,
    cursor: (left) => {
      cursor.hidden = left == null;
      if (left != null) cursor.style.left = `${left}%`;
    },
  };
}

/// An SVG path through `points` ([x, y] in 0–100 by 0–1000 space).
function line(points) {
  if (points.length === 1) {
    const [px, py] = points[0];
    return `M${(px * 10).toFixed(1)} ${py.toFixed(1)} l0.01 0`;
  }
  return points.map(([px, py], i) => `${i ? "L" : "M"}${(px * 10).toFixed(1)} ${py.toFixed(1)}`).join(" ");
}

function tipRows(minute, visible) {
  return visible.map((series) => {
    const point = series.minutes.find((m) => m.minute === minute);
    return { series, point };
  });
}

function fillTip(tip, minute, visible, u) {
  const min = lastPayload.min_profile_deliveries;
  const rows = tipRows(minute, visible).map(({ series, point }) => el("div", { class: `rp-tip-series rp-s-${series.key}` }, [
    el("div", { class: "mx-tip-row strong" }, [
      el("span", { class: "mx-tip-name" }, [
        el("span", { class: "mx-swatch", "aria-hidden": "true" }),
        series.label,
        el("span", { class: series.sparse ? "mx-tip-n low" : "mx-tip-n", text: `×${series.deliveries}` }),
      ]),
      el("span", { class: "mx-tip-value", text: point ? `alive ${fmtPct(point.alive * 100)}` : `ended +${series.horizon}m` }),
    ]),
    point ? el("div", { class: "rp-tip-values" }, [
      el("span", { text: `cpu ${u.cpuText(point.cpu_cores)} ± ${u.cpuText(point.cpu_stddev).replace(/ cores$/, "")}` }),
      el("span", { text: `mem p90 ${u.memText(point.memory_bytes)} · mean ${u.memText(point.memory_mean_bytes)}` }),
    ]) : null,
  ]));
  const sparse = visible.some((series) => series.sparse);
  tip.replaceChildren(
    el("div", { class: "mx-tip-title", text: `+${minute}m since admission` }),
    ...rows,
    sparse ? el("div", { class: "mx-tip-warn", text: `Amber counts: sparse, fewer than ${min} deliveries.` }) : "",
    el("div", { class: "mx-tip-foot", text: "mean ± sd over deliveries still running · ×n = deliveries" }),
  );
}

function tipText(minute, visible, u) {
  return `Minute ${minute}. ` + tipRows(minute, visible).map(({ series, point }) => (point
    ? `${series.label}, ${series.deliveries} deliveries${series.sparse ? ", sparse" : ""}: alive ${fmtPct(point.alive * 100)}, cpu ${u.cpuText(point.cpu_cores)} plus or minus ${u.cpuText(point.cpu_stddev)}, memory p90 ${u.memText(point.memory_bytes)}, mean ${u.memText(point.memory_mean_bytes)}.`
    : `${series.label}: ended at minute ${series.horizon}.`)).join(" ");
}

// ---- table and meta -------------------------------------------------------

function profileTable(all, u) {
  const head = el("tr", {}, ["Series", "Deliveries", "Horizon", "Peak CPU", "Peak memory p90", "Spread cpu · mem", "Build-like"].map((label, i) =>
    el("th", { class: i === 0 ? "" : "num", scope: "col", text: label })));
  const rows = all.map((series) => {
    const p = series.profile;
    const peakCpu = p.peak_cpu_cores || {};
    const peakMem = p.peak_memory_bytes || {};
    return el("tr", { class: series.sparse ? "mx-dim" : "" }, [
      el("th", { scope: "row" }, [
        el("span", { class: `rp-row-name rp-s-${series.key}` }, [el("span", { class: "mx-swatch", "aria-hidden": "true" }), series.label]),
      ]),
      el("td", { class: "num", text: `${series.deliveries}${series.sparse ? " (sparse)" : ""}` }),
      el("td", { class: "num", text: `${series.horizon}m` }),
      el("td", { class: "num", text: `${u.cpuText(peakCpu.value)} @ +${peakCpu.minute ?? 0}m` }),
      el("td", { class: "num", text: `${u.memText(peakMem.value)} @ +${peakMem.minute ?? 0}m` }),
      el("td", { class: "num", text: `${Number(p.cpu_spread).toFixed(2)} · ${Number(p.memory_spread).toFixed(2)}` }),
      el("td", { class: "num", text: p.build_like && p.build_like.length ? `${p.build_like.length} deliveries` : "—" }),
    ]);
  });
  return el("div", { class: "mx-table-wrap", tabindex: "0", role: "region", "aria-label": "Profile summary" }, [
    el("table", { class: "mx-table" }, [
      el("caption", { text: "Per profile: peaks before the horizon; spread is per-minute standard deviation over mean (near 0 forecasts well, near 1 does not)" }),
      el("thead", {}, [head]),
      el("tbody", {}, rows),
    ]),
  ]);
}

function metaLine() {
  const p = lastPayload;
  const parts = [
    `built ${formatDateTime(p.built_at)}`,
    `each profile: last ${p.profile_window} completed deliveries of its kind within 30 days`,
    `sparse below ${p.min_profile_deliveries}`,
    p.host ? hostText(p.host) : "host size unavailable",
    `admit at ${p.admit_cpu_percent}% cpu · ${p.admit_memory_percent}% memory`,
  ];
  return el("p", { class: "rp-meta", text: parts.join(" · ") });
}

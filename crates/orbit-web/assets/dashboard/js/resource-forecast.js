// Orbit dashboard Analytics › Forecast › Resource forecast: whether the next
// task fits this host's CPU and memory, the forecast `machine.auto_concurrency`
// admits with (off, shadow or enforce). Read-only; nothing here builds a
// forecast of its own. Three reads, made together when the view first opens
// and on each refresh:
//   /api/diagnostics/resource_forecast  the configured mode, host size, admit
//     thresholds and the accuracy report (`orbit run profiles --accuracy`);
//   /api/resource-profiles              the delivery profiles admission prices
//     with (`orbit run profiles`);
//   /api/workflows/auto/readiness       the drain's mode and its newest
//     recorded admission pass, drawn as the Tasks drain card draws it.
// The Profiles, Accuracy and Decisions tabs stay rendered; a switch only shows
// another, so it never refetches.

import {
  syncNodes, fetchJson, requestPanel, panelCanRender, getWorkspaceRevision, onWorkspaceChange,
  renderPanelPlaceholder, isAggregateView, formatDateTime, relativeTime,
} from './common.js';
import { el, forecastCard, forecastTable } from './forecast-shared.js';
import { autoDrainForecast } from './operations-forecast.js';
import { runLink } from './operations-shared.js';

const $ = (id) => document.getElementById(id);

const BODY = "resource-forecast-body";
const COUNT = "resource-forecast-count";
const FORECAST_ENDPOINT = "/api/diagnostics/resource_forecast";
const PROFILES_ENDPOINT = "/api/resource-profiles";
const READINESS_ENDPOINT = "/api/workflows/auto/readiness";
const GIB = 1024 ** 3;
const TABS = [["profiles", "Profiles"], ["accuracy", "Accuracy"], ["decisions", "Decisions"]];
const DEFAULT_TAB = TABS[0][0];
const FILTERS = [["all", "All"], ["change", "Change"], ["no-diff", "No-diff"], ["auto-task", "Auto-tasks"], ["sparse", "Sparse"]];
const COMPLEXITY_ORDER = ["low", "medium", "hard", "xhard"];
const TEMPLATE_PREFIX = "auto-task:";
const MODES = ["off", "shadow", "enforce"];

const view = { tab: DEFAULT_TAB, filter: "all", payload: null, revision: null };
let tabPickListener = null;

onWorkspaceChange(() => {
  view.payload = null;
  view.revision = null;
});

/// True once this workspace's resource forecast has been read, so opening the
/// view again shows it without a read.
export function resourceForecastLoaded() {
  return view.payload != null && view.revision === getWorkspaceRevision();
}

export function placeholdResourceForecastAggregate() {
  view.payload = null;
  const count = $(COUNT);
  if (count) count.textContent = "—";
  renderPanelPlaceholder(BODY);
}

export function fetchAndRenderResourceForecast() {
  if (isAggregateView()) {
    placeholdResourceForecastAggregate();
    return Promise.resolve();
  }
  const revision = getWorkspaceRevision();
  return requestPanel(BODY, FORECAST_ENDPOINT, async () => {
    // The drain card's read is optional here: a host without one still has
    // profiles and accuracy to show.
    const [forecast, profiles, readiness] = await Promise.all([
      fetchJson(FORECAST_ENDPOINT),
      fetchJson(PROFILES_ENDPOINT),
      fetchJson(READINESS_ENDPOINT).catch(() => null),
    ]);
    return { forecast, profiles, readiness };
  }, (payload) => {
    if (getWorkspaceRevision() !== revision) return;
    view.payload = payload;
    view.revision = revision;
    renderResourceForecast();
  }, COUNT);
}

export function renderResourceForecast() {
  const body = $(BODY);
  if (!body) return;
  if (isAggregateView()) {
    placeholdResourceForecastAggregate();
    return;
  }
  if (!view.payload || !panelCanRender(BODY)) return;
  const { forecast, profiles } = view.payload;
  const count = $(COUNT);
  const rows = Array.isArray(profiles?.profiles) ? profiles.profiles : [];
  if (count) count.textContent = `${modeOf(view.payload).mode} · ${rows.length} profile${rows.length === 1 ? "" : "s"}`;
  const root = el("div", { class: "fc-view rf-view" }, [
    headerLine(),
    settingsCards(),
    tabStrip(),
    tabPanel("profiles", profilesPanel(rows)),
    tabPanel("accuracy", accuracyPanel(forecast?.accuracy)),
    tabPanel("decisions", decisionsPanel(forecast?.accuracy)),
  ]);
  root.dataset.key = "resource-forecast-view";
  // An unchanged poll keeps the nodes on screen; new figures or a filter
  // replace them.
  root.dataset.hash = JSON.stringify([view.filter, view.payload]);
  syncNodes(body, [root]);
  applyTab();
}

// ---- tabs -----------------------------------------------------------------

/// The route of the view on screen, without the leading `#`.
export function resourceForecastRoute() {
  return view.tab === DEFAULT_TAB ? "analytics/forecast/resources" : `analytics/forecast/resources/${view.tab}`;
}

/// Show `tab` (an unknown or missing one is Profiles).
export function setResourceForecastTab(tab) {
  view.tab = TABS.some(([key]) => key === tab) ? tab : DEFAULT_TAB;
  applyTab();
}

/// `listener` runs when the operator picks a tab, so the router can keep the
/// address in step without routing, which would refetch.
export function onResourceForecastTabPick(listener) {
  tabPickListener = listener;
}

function pickTab(tab) {
  setResourceForecastTab(tab);
  tabPickListener?.();
}

function applyTab() {
  const body = $(BODY);
  if (!body) return;
  for (const button of body.querySelectorAll(".rf-tab")) {
    const on = button.dataset.rfTab === view.tab;
    button.classList.toggle("active", on);
    button.setAttribute("aria-selected", on ? "true" : "false");
    button.tabIndex = on ? 0 : -1;
  }
  for (const node of body.querySelectorAll(".rf-tabbed")) node.hidden = node.dataset.rfTab !== view.tab;
}

function tabStrip() {
  const strip = el("div", { class: "band-tabs fc-tabs", role: "tablist", "aria-label": "Resource forecast sections" }, TABS.map(([key, text]) => {
    const button = el("button", { id: `rf-tab-${key}`, class: "band-tab rf-tab", type: "button", role: "tab", "aria-controls": `rf-panel-${key}`, text });
    button.dataset.rfTab = key;
    button.addEventListener("click", () => pickTab(key));
    return button;
  }));
  strip.addEventListener("keydown", (event) => {
    const keys = TABS.map(([key]) => key);
    const index = keys.indexOf(event.target?.dataset?.rfTab);
    if (index < 0) return;
    let next = null;
    if (event.key === "ArrowRight") next = (index + 1) % keys.length;
    else if (event.key === "ArrowLeft") next = (index - 1 + keys.length) % keys.length;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = keys.length - 1;
    if (next == null) return;
    event.preventDefault();
    pickTab(keys[next]);
    $(`rf-tab-${keys[next]}`)?.focus();
  });
  return strip;
}

function tabPanel(key, children) {
  const panel = el("div", { id: `rf-panel-${key}`, class: "fc-panel rf-tabbed", role: "tabpanel", "aria-labelledby": `rf-tab-${key}` }, children);
  panel.dataset.rfTab = key;
  return panel;
}

// ---- header and settings ----------------------------------------------------

// The mode admission runs in: the drain's own (an explicit mode it started
// with, or the host's setting it inherits), else the host's setting.
function modeOf(payload) {
  const configured = String(payload.forecast?.configured_mode || "off");
  const drain = payload.readiness?.auto_concurrency;
  const runId = payload.readiness?.capacity?.drain_run_id || null;
  if (drain?.mode && drain.source === "explicit") {
    return { mode: String(drain.mode), source: `set on drain ${runId || "start"}`, configured, runId };
  }
  return { mode: configured, source: "machine.auto_concurrency.mode on this host", configured, runId };
}

function headerLine() {
  const built = view.payload.profiles?.built_at;
  const age = relativeTime(built);
  const text = `Profiles built ${age ?? "earlier"} · drives machine.auto_concurrency · read-only`;
  const meta = el("p", { class: "fc-meta", text, title: built ? `Built ${formatDateTime(built)}` : null });
  const refresh = el("button", { class: "fc-refresh", type: "button", title: "Read the resource forecast again", "aria-label": "Refresh resource forecast" }, [
    el("span", { class: "visually-hidden", text: "Refresh" }),
  ]);
  refresh.addEventListener("click", () => {
    refresh.disabled = true;
    fetchAndRenderResourceForecast()
      .catch((error) => console.error("Failed to refresh the resource forecast", error))
      .finally(() => { refresh.disabled = false; });
  });
  return el("div", { class: "fc-header" }, [meta, el("div", { class: "fc-tools" }, [refresh])]);
}

function settingCard(label, value, note, extra = null) {
  return el("div", { class: "rf-setting" }, [
    el("div", { class: "rf-setting-label", text: label }),
    value,
    extra,
    note ? el("div", { class: "rf-setting-note", text: note }) : null,
  ]);
}

function settingsCards() {
  const { forecast } = view.payload;
  const mode = modeOf(view.payload);
  const pills = el("div", { class: "rf-mode-pills", "aria-hidden": "true" }, MODES.map((name) =>
    el("span", { class: `rf-mode-pill${name === mode.mode ? ` on ${name}` : ""}`, text: name })));
  const modeValue = el("div", { class: "rf-setting-value" }, [
    el("span", { class: `rf-mode-dot ${mode.mode}`, "aria-hidden": "true" }),
    el("span", { text: mode.mode.charAt(0).toUpperCase() + mode.mode.slice(1) }),
  ]);
  const modeNote = mode.mode !== mode.configured ? `${mode.source} · host setting ${mode.configured}` : mode.source;
  const host = forecast?.host;
  const cpu = Number(host?.cpu_count);
  const memory = Number(host?.memory_total_bytes);
  const measured = Number.isFinite(cpu) && cpu > 0 && Number.isFinite(memory) && memory > 0;
  const admitCpu = Number(forecast?.admit_cpu_percent);
  const admitMemory = Number(forecast?.admit_memory_percent);
  const limits = measured
    ? `≤ ${(cpu * admitCpu / 100).toFixed(1)} cores · ≤ ${(memory * admitMemory / 100 / GIB).toFixed(1)} GiB forecast peak`
    : "Host size unknown: every candidate is admitted";
  const live = liveBaseline();
  return el("div", { class: "rf-settings" }, [
    settingCard("Mode", modeValue, modeNote, pills),
    settingCard("Host capacity", el("div", { class: "rf-setting-value mono", text: measured ? `${cpu} cores · ${(memory / GIB).toFixed(0)} GiB` : "Not measured" }), "This host, where admission runs"),
    settingCard("Admit thresholds", el("div", { class: "rf-setting-value mono", text: `CPU ${admitCpu}% · Mem ${admitMemory}%` }), limits),
    el("div", { class: "rf-setting rf-live" }, [el("div", { class: "rf-setting-label", text: "Live baseline" }), live]),
  ]);
}

// The drain's newest recorded admission pass, as the Tasks drain card shows
// it. Its backlog summary belongs to that card, not here.
function liveBaseline() {
  const readiness = view.payload.readiness;
  if (!readiness) return el("p", { class: "rf-setting-note", text: "Drain readiness is unavailable on this host." });
  const section = autoDrainForecast(readiness);
  section.querySelector(":scope > .drain-backlog-forecast")?.remove();
  return section;
}

// ---- Profiles -----------------------------------------------------------------

function profileKind(profile) {
  const workClass = String(profile?.key?.work_class || "");
  if (workClass.startsWith(TEMPLATE_PREFIX)) return "auto-task";
  return workClass === "no-diff" ? "no-diff" : "change";
}

function compareProfiles(a, b) {
  const ka = profileKind(a);
  const kb = profileKind(b);
  if (ka !== kb) return FILTERS.findIndex(([key]) => key === ka) - FILTERS.findIndex(([key]) => key === kb);
  const wa = String(a?.key?.work_class || "");
  const wb = String(b?.key?.work_class || "");
  if (wa !== wb) return wa.localeCompare(wb);
  const ca = COMPLEXITY_ORDER.indexOf(a?.key?.complexity);
  const cb = COMPLEXITY_ORDER.indexOf(b?.key?.complexity);
  return (ca < 0 ? 99 : ca) - (cb < 0 ? 99 : cb);
}

function cores(value) {
  const number = Number(value);
  return Number.isFinite(number) ? number.toFixed(1) : "—";
}

function gib(value) {
  const number = Number(value);
  return Number.isFinite(number) ? (number / GIB).toFixed(1) : "—";
}

function peakText(peak, format, unit) {
  if (!peak || !Number.isFinite(Number(peak.value))) return "—";
  return `${format(peak.value)}${unit} @ +${peak.minute ?? "?"}m`;
}

function quantileText(pair, format) {
  if (!pair) return "—";
  return `${format(pair.p50)} / ${format(pair.p90)}`;
}

function profileRow(profile) {
  const sparse = profile.sparse === true;
  const builds = Array.isArray(profile.build_like) ? profile.build_like.length : 0;
  const count = el("td", { class: "num" }, [
    String(profile.delivery_count ?? "—"),
    sparse ? el("span", { class: "rf-badge", text: "sparse", title: "Fewer deliveries than admission needs; it prices this key with its base class" }) : null,
  ]);
  return el("tr", { class: sparse ? "rf-sparse" : null }, [
    el("th", { scope: "row", text: String(profile.key?.work_class || "—") }),
    el("td", { text: profile.key?.complexity || "—" }),
    count,
    el("td", { class: "num", text: Number.isFinite(Number(profile.horizon_minutes)) ? `${profile.horizon_minutes}m` : "—" }),
    el("td", { class: "num", text: peakText(profile.peak_cpu_cores, cores, "") }),
    el("td", { class: "num", text: quantileText(profile.delivery_peak_cpu, cores) }),
    el("td", { class: "num", text: peakText(profile.peak_memory_bytes, gib, " GiB") }),
    el("td", { class: "num", text: quantileText(profile.delivery_peak_memory_bytes, gib) }),
    el("td", { class: "num", text: `cpu ${Number(profile.cpu_spread ?? NaN).toFixed(2)} · mem ${Number(profile.memory_spread ?? NaN).toFixed(2)}` }),
    el("td", { text: builds ? `build-like (${builds})` : "—" }),
  ]);
}

function filterControl(total) {
  const group = el("div", { class: "mx-seg mx-seg-wrap", role: "group", "aria-label": "Filter profiles" }, FILTERS.map(([key, text]) => {
    const button = el("button", { class: `mx-seg-btn${key === view.filter ? " on" : ""}`, type: "button", text, "aria-pressed": key === view.filter ? "true" : "false" });
    button.addEventListener("click", () => {
      if (view.filter === key) return;
      view.filter = key;
      renderResourceForecast();
    });
    return button;
  }));
  return el("div", { class: "fc-header" }, [
    el("p", { class: "fc-meta", text: `Peak CPU and memory each kind of delivery is forecast to use, by minute after admission · ${total} profile${total === 1 ? "" : "s"}` }),
    group,
  ]);
}

function profilesPanel(rows) {
  if (rows.length === 0) {
    return [el("p", { class: "fc-line", text: "No delivery profiles yet: a profile needs a delivery this host measured. Until then admission prices candidates without one." })];
  }
  const shown = rows
    .filter((profile) => view.filter === "all" || (view.filter === "sparse" ? profile.sparse === true : profileKind(profile) === view.filter))
    .sort(compareProfiles);
  const link = el("a", { href: "#analytics/resources", text: "Analytics › Resources" });
  return [
    filterControl(rows.length),
    forecastTable("Delivery profiles", [
      ["Work class"], ["Complexity"], ["Deliveries", true], ["Horizon", true], ["Peak CPU", true], ["CPU p50 / p90", true],
      ["Peak mem", true], ["Mem p50 / p90 GiB", true], ["Spread", true], ["Build-like"],
    ], shown.length ? shown.map(profileRow) : [el("tr", {}, [el("td", { colspan: "10", text: "No profile matches this filter." })])]),
    el("p", { class: "fc-line rf-foot" }, [
      "Peak is the profile's highest minute and when it lands after admission; p50 / p90 are per-delivery peaks. A sparse profile has fewer than ",
      String(view.payload.forecast?.min_profile_deliveries ?? "the minimum"),
      " deliveries and falls back to its base class. Curves are in ",
      link,
      "; same data as orbit run profiles.",
    ]),
  ];
}

// ---- Accuracy -------------------------------------------------------------------

function signed(value, digits, unit = "") {
  const number = Number(value);
  if (value == null || !Number.isFinite(number)) return "—";
  const text = Math.abs(number).toFixed(digits);
  return `${number < 0 ? "−" : number > 0 ? "+" : ""}${text}${unit}`;
}

function share(value) {
  const number = Number(value);
  return value == null || !Number.isFinite(number) ? "—" : `${Math.round(number * 100)}%`;
}

function underCell(value) {
  // A forecast under the measured load more often than not admits too much.
  const high = Number(value) > 0.6;
  return el("td", { class: `num${high ? " bad" : ""}`, text: share(value) });
}

function offsetRow(offset) {
  const cpu = offset.cpu || {};
  const memory = offset.memory || {};
  return el("tr", {}, [
    el("th", { scope: "row", text: `+${offset.offset_minutes}m` }),
    el("td", { class: "num", text: Number(offset.paired ?? 0).toLocaleString() }),
    el("td", { class: "num", text: Number(offset.unmeasured ?? 0).toLocaleString(), title: `${offset.pending ?? 0} pending · ${offset.missing ?? 0} missing` }),
    el("td", { class: "num", text: signed(cpu.mean_error, 1) }),
    el("td", { class: "num", text: signed(cpu.mean_absolute_error, 1).replace(/^\+/, "") }),
    underCell(cpu.under_rate),
    el("td", { class: "num", text: signed(memory.mean_error == null ? null : memory.mean_error / GIB, 1, " GiB") }),
    el("td", { class: "num", text: signed(memory.mean_absolute_error == null ? null : memory.mean_absolute_error / GIB, 1, " GiB").replace(/^\+/, "") }),
    underCell(memory.under_rate),
  ]);
}

// The largest miss across offsets for one resource.
function worstMiss(offsets, resource) {
  let worst = null;
  for (const offset of offsets) {
    const miss = offset?.[resource]?.worst_miss;
    if (miss && (!worst || Math.abs(Number(miss.error)) > Math.abs(Number(worst.miss.error)))) worst = { offset: offset.offset_minutes, miss };
  }
  return worst;
}

function missLine(label, worst, format) {
  if (!worst) return null;
  const { miss, offset } = worst;
  return el("div", { class: "rf-miss" }, [
    el("div", { class: "rf-setting-label", text: `${label} at +${offset}m` }),
    el("div", { class: "mono rf-miss-text" }, [
      `${format(miss.error, true)} · forecast ${format(miss.forecast)}, measured ${format(miss.measured)} · ${formatDateTime(miss.minute_at)} · `,
      runLink(String(miss.run_id), null),
    ]),
  ]);
}

function accuracyPanel(accuracy) {
  const offsets = Array.isArray(accuracy?.offsets) ? accuracy.offsets : [];
  if (!accuracy || Number(accuracy.forecasts ?? 0) === 0 || offsets.length === 0) {
    return [el("p", { class: "fc-line", text: "No admission forecasts recorded on this host yet. A drain records one on each pass while machine.auto_concurrency is shadow or enforce." })];
  }
  const since = accuracy.since ? new Date(accuracy.since).toLocaleDateString(undefined, { month: "short", day: "numeric" }) : "the start";
  const cpuFormat = (value, sign) => (sign ? signed(value, 1, " cores") : `${Number(value).toFixed(1)}`);
  const memFormat = (value, sign) => (sign ? signed(Number(value) / GIB, 1, " GiB") : `${(Number(value) / GIB).toFixed(1)} GiB`);
  return [
    el("p", { class: "fc-meta", text: `${Number(accuracy.forecasts).toLocaleString()} forecasts on this host since ${since} · error = forecast − measured` }),
    forecastTable("Forecast accuracy by offset", [
      ["Offset"], ["Paired", true], ["Unmeasured", true], ["CPU mean err", true], ["CPU MAE", true], ["CPU under", true],
      ["Mem mean err", true], ["Mem MAE", true], ["Mem under", true],
    ], offsets.map(offsetRow)),
    el("div", { class: "rf-misses" }, [
      missLine("Worst CPU miss", worstMiss(offsets, "cpu"), cpuFormat),
      missLine("Worst memory miss", worstMiss(offsets, "memory"), memFormat),
    ].filter(Boolean)),
    el("p", { class: "fc-line rf-foot", text: "Under is the share of paired minutes whose measured load was above the forecast; above 60% is marked. CPU in cores. Same data as orbit run profiles --accuracy." }),
  ];
}

// ---- Decisions ----------------------------------------------------------------

function drainMode(drain) {
  const shadow = Number(drain.shadow_passes) > 0;
  const enforce = Number(drain.enforce_passes) > 0;
  if (shadow && enforce) return "mixed";
  return shadow ? "shadow" : enforce ? "enforce" : "—";
}

function idsCell(ids, applies) {
  if (!applies) return el("td", { class: "num rf-na", text: "—" });
  const list = Array.isArray(ids) ? ids : [];
  return el("td", { class: `num${list.length ? " bad" : ""}`, text: String(list.length), title: list.length ? list.join(", ") : null });
}

function decisionRow(drain, showWorkspace, showPull) {
  const shadow = Number(drain.shadow_passes) > 0;
  const enforce = Number(drain.enforce_passes) > 0;
  const cells = [
    el("th", { scope: "row" }, [runLink(String(drain.run_id), null)]),
    showWorkspace ? el("td", { text: String(drain.workspace_id || "—") }) : null,
    el("td", { text: drainMode(drain) }),
    el("td", { class: "num", text: String(Number(drain.shadow_passes ?? 0) + Number(drain.enforce_passes ?? 0)) }),
    el("td", { class: "num", text: String(Number(drain.admitted ?? 0) + Number(drain.enforce_admitted ?? 0)) }),
    idsCell(drain.gap_held, enforce),
    idsCell(drain.would_hold, shadow),
    idsCell(drain.would_add, shadow),
  ];
  if (showPull) {
    cells.push(
      el("td", { class: "num", text: `${drain.pull_sent ?? 0} / ${drain.pull_requests ?? 0}` }),
      el("td", { class: "num", text: String(Number(drain.pull_held ?? 0) + Number(drain.pull_would_hold ?? 0)) }),
    );
  }
  return el("tr", {}, cells);
}

function decisionsPanel(accuracy) {
  const drains = (Array.isArray(accuracy?.drains) ? accuracy.drains : [])
    .filter((drain) => Number(drain.shadow_passes) + Number(drain.enforce_passes) + Number(drain.pull_requests) > 0)
    .sort((a, b) => String(b.run_id).localeCompare(String(a.run_id)));
  if (drains.length === 0) {
    return [el("p", { class: "fc-line", text: "No drain has made an admission decision on this host yet. Each shadow or enforce pass is recorded here." })];
  }
  const showWorkspace = new Set(drains.map((drain) => drain.workspace_id)).size > 1;
  const showPull = drains.some((drain) => Number(drain.pull_requests) > 0);
  const head = [["Drain run"], ...(showWorkspace ? [["Workspace"]] : []), ["Mode"], ["Passes", true], ["Admitted", true], ["Held", true], ["Would hold", true], ["Would add", true],
    ...(showPull ? [["Pull sent", true], ["Pull held", true]] : [])];
  return [
    el("p", { class: "fc-meta", text: `${drains.length} drain${drains.length === 1 ? "" : "s"} with forecast passes, newest first` }),
    forecastTable("Admission decisions per drain", head, drains.map((drain) => decisionRow(drain, showWorkspace, showPull))),
    el("p", { class: "fc-line rf-foot", text: "Enforce passes hold a task whose forecast crosses an admit threshold (Held). Shadow passes admit as before and record what enforce would have held or added. Hover a count for its task ids; a run opens its recorded admission pass." }),
  ];
}

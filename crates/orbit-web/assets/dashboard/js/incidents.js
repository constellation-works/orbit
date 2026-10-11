// Orbit dashboard Health › Incidents [ORB-10871, ORB-15221].
//
// A raw failed audit row is evidence; an incident is one problem. The page
// answers "which failures are happening, how often, and since when":
//
// - class tiles count each failure class over the window and pick the class
//   the list shows (the server filters by class before its row limit);
// - the list rolls a signature up across runs and draws each one's first to
//   last sighting on an axis shared by every row and the selected window;
// - the detail panel shows the selected signature's full message, its runs
//   and its underlying audit events, and links out to both.
//
// Every count states what it is out of and which window it covers. The
// grouping signature is an internal key: it decides which rows roll up and
// which other signatures share a message, but it is never shown as one.

import { incidentClassLabel, auditActorLabel, el, syncNodes, getWindow, setWindow, notifyScopeChange, DASHBOARD_WINDOWS, getWorkspace, formatDateTime, runHref, bindTaskLink, makeCopyButton } from './common.js';
import { navigateToDrilldown } from './audit.js';

const $ = (id) => document.getElementById(id);

const CLASS_ORDER = ["unexpected", "expected", "denied", "diagnostic", "deferred"];
const CLASS_TITLES = {
  unexpected: "Unexpected failures",
  expected: "Expected refusals",
  denied: "Policy denials",
  diagnostic: "Lifecycle diagnostics",
  deferred: "Deferred, not failed",
};
const CLASS_NAMES = {
  unexpected: "Unexpected failure",
  expected: "Expected refusal",
  denied: "Policy denial",
  diagnostic: "Lifecycle diagnostic",
  deferred: "Deferred, not failed",
};
// The shared dashboard windows this page offers, plus 6h, which only it
// offers. 6h is kept here rather than in the shared scope, so it never
// reaches views whose endpoints only know the shared windows.
const INCIDENT_WINDOWS = ["1h", "6h", "24h", "7d"];
const MINUTE_MS = 60 * 1000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;
const INCIDENT_PAGE = 50;
const INCIDENT_PAGE_MAX = 200;
const RELATED_MAX = 5;

let incidentClass = "unexpected";
let windowOverride = null;
let searchQuery = "";
let lastHourOnly = false;
let noMessageOnly = false;
let selectedKey = null;
let runsOpen = false;
let evidenceOpen = false;
let lastPayload = null;
let lastCtx = {};
let layout = null;

export function getIncidentClass() { return incidentClass; }

/// The window the incidents request covers: the shared dashboard window,
/// unless this page picked 6h since the shared window last changed.
export function getIncidentWindow() {
  if (windowOverride && windowOverride.base === getWindow()) return windowOverride.value;
  windowOverride = null;
  return getWindow();
}

function hasCtx(ctx, key) {
  return ctx && typeof ctx[key] === "function";
}

function fmtRelativeValue(ctx, v) {
  return hasCtx(ctx, "fmtRelative") ? ctx.fmtRelative(v) : (v || "-");
}

function truncateValue(ctx, s, n) {
  return hasCtx(ctx, "truncate") ? ctx.truncate(s, n) : String(s || "").slice(0, n);
}

function asCount(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

function plural(count, noun) {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

function eventCountLabel(value) {
  return plural(asCount(value), "event");
}

function number(value) {
  return asCount(value).toLocaleString("en-US");
}

function plainClick(event) {
  return !(event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || event.button !== 0);
}

function windowPhrase(window) {
  return window === "all" ? "over all time" : `in ${window}`;
}

// ===== Messages. A row with no recorded message says so rather than showing
// the grouping signature (`unexpected|role=…|msg=…`).

const TRAILING_ELLIPSIS = /(?:…|\.\.\.)\s*$/;

function recordedMessage(row) {
  return typeof row.message === "string" ? row.message.trim() : "";
}

function incidentMessageText(row) {
  return recordedMessage(row) || `${row.surface || "unknown surface"} failed; no message recorded`;
}

// The payload has no per-incident truncation flag. A message that ends in an
// ellipsis was cut before it was stored; when one of the incident's own audit
// events carries a longer copy of the same text, that copy is the message.
function fullMessage(incidents) {
  const sample = incidents[0];
  const recorded = recordedMessage(sample);
  if (!recorded) return { text: incidentMessageText(sample), recorded: false, truncated: false };
  if (!TRAILING_ELLIPSIS.test(recorded)) return { text: recorded, recorded: true, truncated: false };
  const stem = recorded.replace(TRAILING_ELLIPSIS, "");
  for (const incident of incidents) {
    for (const event of incidentEvents(incident)) {
      const message = recordedMessage(event);
      if (message.length > stem.length && message.startsWith(stem) && !TRAILING_ELLIPSIS.test(message)) {
        return { text: message, recorded: true, truncated: false };
      }
    }
  }
  return { text: recorded, recorded: true, truncated: true };
}

// A row title is the message's first line; the detail panel shows all of it.
function headline(text) {
  const first = String(text).split("\n").find(line => line.trim()) || String(text);
  return first.trim();
}

// The normalized message part of a signature, for "same message" matching.
function signatureMessage(incident) {
  const signature = typeof incident.signature === "string" ? incident.signature : "";
  const index = signature.indexOf("|msg=");
  return index >= 0 ? signature.slice(index + 5).trim() : "";
}

function incidentEvents(incident) {
  if (Array.isArray(incident.events) && incident.events.length) return incident.events;
  return [
    ...(Array.isArray(incident.sample_events) ? incident.sample_events : []),
    ...((Array.isArray(incident.propagation) ? incident.propagation : [])
      .flatMap((link) => Array.isArray(link.sample_events) ? link.sample_events : [])),
  ];
}

// ===== Cross-run rollup [ORB-15221]. The store keys an incident by (run
// scope, signature), so the same failure in a later run is a new incident.
// The signature is that shared identity. An incident with no signature stays
// on its own: grouping those would merge rows the server could not identify.

function incidentGroupKey(incident) {
  const signature = typeof incident.signature === "string" ? incident.signature.trim() : "";
  if (!signature) return "";
  return `${incident.class || "unexpected"}|${signature}`;
}

function incidentInstant(incident, field) {
  const time = Date.parse(incident[field] || "");
  return Number.isFinite(time) ? time : null;
}

function incidentRunIds(incident) {
  if (!Array.isArray(incident.run_ids)) return [];
  return incident.run_ids
    .map(id => (id == null ? "" : String(id).trim()))
    .filter(Boolean);
}

function distinctRunCount(incidents) {
  const ids = new Set();
  for (const incident of incidents) {
    for (const id of incidentRunIds(incident)) ids.add(id);
  }
  return ids.size;
}

function recurrenceCountLabel(incidents) {
  const runs = distinctRunCount(incidents);
  if (runs >= 2) return `${runs} runs`;
  if (runs === 1) return `${incidents.length} incidents · 1 run`;
  return `${incidents.length} incidents`;
}

function formatCadence(ms) {
  if (!Number.isFinite(ms) || ms <= 0) return "";
  if (ms >= DAY_MS * 0.75) {
    const days = Math.max(1, Math.round(ms / DAY_MS));
    if (Math.abs(ms - days * DAY_MS) <= 30 * MINUTE_MS) return `every ${days} d`;
  }
  if (ms >= HOUR_MS * 0.75) {
    const hours = Math.max(1, Math.round(ms / HOUR_MS));
    if (Math.abs(ms - hours * HOUR_MS) <= 90 * 1000) return `every ${hours} h`;
  }
  if (ms >= MINUTE_MS * 0.75) {
    const minutes = Math.max(1, Math.round(ms / MINUTE_MS));
    return `every ${minutes} min`;
  }
  return `every ${Math.max(1, Math.round(ms / 1000))} s`;
}

function medianGap(gaps) {
  const mid = Math.floor(gaps.length / 2);
  if (gaps.length % 2 === 1) return gaps[mid];
  return (gaps[mid - 1] + gaps[mid]) / 2;
}

// Cadence is the median gap between fires. A spread within 25% of that median
// reads as a schedule (`every 20 min`); a wider spread is qualified.
function cadenceLabel(incidents) {
  const times = incidents
    .map(incident => incidentInstant(incident, "last_ts") ?? incidentInstant(incident, "first_ts"))
    .filter(time => time != null)
    .sort((a, b) => a - b);
  const gaps = [];
  for (let index = 1; index < times.length; index += 1) {
    const gap = times[index] - times[index - 1];
    if (gap > 0) gaps.push(gap);
  }
  if (gaps.length === 0) return "";
  gaps.sort((a, b) => a - b);
  const median = medianGap(gaps);
  const phrase = formatCadence(median);
  if (!phrase) return "";
  const tight = gaps[0] >= median * 0.75 && gaps[gaps.length - 1] <= median * 1.25;
  return tight ? phrase : phrase.replace(/^every /, "about every ");
}

function sharedIncidentActor(incidents) {
  const actors = [...new Set(incidents.map(incident => (typeof incident.actor === "string" ? incident.actor : "")))];
  return actors.length === 1 ? actors[0] : null;
}

function actorLabel(incidents) {
  const actor = sharedIncidentActor(incidents);
  if (actor == null) return "several actors";
  return actor ? auditActorLabel(actor) : "unknown actor";
}

function groupBounds(incidents) {
  let first = null;
  let last = null;
  for (const incident of incidents) {
    const start = incidentInstant(incident, "first_ts") ?? incidentInstant(incident, "last_ts");
    const end = incidentInstant(incident, "last_ts") ?? incidentInstant(incident, "first_ts");
    if (start != null && (!first || start < first.time)) first = { time: start, text: incident.first_ts || incident.last_ts };
    if (end != null && (!last || end > last.time)) last = { time: end, text: incident.last_ts || incident.first_ts };
  }
  return { first, last };
}

/// One list row per signature, newest last-seen first. Each run of a recurring
/// signature stays its own incident beneath it, newest first.
function groupIncidents(incidents) {
  const groups = [];
  const byKey = new Map();
  incidents.forEach((incident, index) => {
    const key = incidentGroupKey(incident);
    if (!key) {
      groups.push({ key: `single:${incident.incident_id || index}`, incidents: [incident], order: index });
      return;
    }
    let group = byKey.get(key);
    if (!group) {
      group = { key: `signature:${key}`, incidents: [], order: index };
      byKey.set(key, group);
      groups.push(group);
    }
    group.incidents.push(incident);
  });
  for (const group of groups) {
    group.incidents.sort((a, b) => {
      const byTime = (incidentInstant(b, "last_ts") || 0) - (incidentInstant(a, "last_ts") || 0);
      if (byTime !== 0) return byTime;
      return String(a.incident_id || "").localeCompare(String(b.incident_id || ""));
    });
    const sample = group.incidents[0];
    const bounds = groupBounds(group.incidents);
    const message = fullMessage(group.incidents);
    Object.assign(group, {
      recurring: group.incidents.length > 1,
      sample,
      cls: sample.class || "unexpected",
      first: bounds.first ? bounds.first.text : "",
      last: bounds.last ? bounds.last.text : "",
      firstMs: bounds.first ? bounds.first.time : null,
      lastMs: bounds.last ? bounds.last.time : null,
      events: group.incidents.reduce((sum, incident) => sum + asCount(incident.event_count), 0),
      runs: distinctRunCount(group.incidents),
      message,
      title: headline(message.text),
      sameMessage: signatureMessage(sample) || (message.recorded ? message.text : ""),
    });
  }
  return groups.sort((a, b) => (b.lastMs ?? 0) - (a.lastMs ?? 0) || a.order - b.order);
}

function listCap(payload) {
  const shown = asCount(payload && payload.shown_incident_count);
  const matching = asCount(payload && payload.matching_incident_count);
  return { capped: shown > 0 && matching > shown, shown, matching };
}

function countLabel(group) {
  return group.recurring ? recurrenceCountLabel(group.incidents) : eventCountLabel(group.events);
}

function countTitle(group, cap) {
  const parts = [`${eventCountLabel(group.events)} collapsed into ${plural(group.incidents.length, "incident")}`];
  if (group.recurring && cap.capped) parts.push(`run count is among the newest ${cap.shown} of ${cap.matching} incidents`);
  return parts.join(" · ");
}

// ===== The shared window axis. It runs from the payload's `since` to now;
// a lifetime (`all`) window starts at the oldest first sighting shown.

function windowSpanMs(label) {
  const match = /^(\d+)([mhdw])$/.exec(String(label || ""));
  if (!match) return null;
  return Number(match[1]) * { m: MINUTE_MS, h: HOUR_MS, d: DAY_MS, w: 7 * DAY_MS }[match[2]];
}

function incidentAxis(payload, groups, now) {
  let start = Date.parse(payload.since || "");
  if (!Number.isFinite(start)) {
    const span = windowSpanMs(payload.window || getIncidentWindow());
    if (span) start = now - span;
  }
  if (!Number.isFinite(start)) {
    const firsts = groups.map(group => group.firstMs).filter(Number.isFinite);
    start = firsts.length ? Math.min(...firsts) : now - DAY_MS;
  }
  const end = Math.max(now, start + MINUTE_MS);
  return { start, end, span: end - start };
}

function spanLabel(ms) {
  const tenth = (value) => String(Math.round(value * 10) / 10);
  if (ms < HOUR_MS) return `${Math.round(ms / MINUTE_MS)}m`;
  if (ms < 2 * DAY_MS) return `${tenth(ms / HOUR_MS)}h`;
  return `${tenth(ms / DAY_MS)}d`;
}

function axisLabels(axis, parts) {
  const labels = [];
  for (let index = 0; index <= parts; index += 1) {
    const ago = axis.span * (parts - index) / parts;
    labels.push(index === 0 ? `${spanLabel(ago)} ago` : index === parts ? "now" : spanLabel(ago));
  }
  return labels;
}

// Percent offsets from the axis start. A lifespan narrower than 1% of the
// axis is a single burst and draws as a dot at its midpoint.
function lifespanMark(group, axis) {
  const clamp = (value) => Math.min(100, Math.max(0, value));
  const at = (time) => clamp(((time ?? axis.end) - axis.start) / axis.span * 100);
  const left = at(group.firstMs ?? group.lastMs);
  const right = at(group.lastMs ?? group.firstMs);
  const width = Math.max(0, right - left);
  return width < 1 ? { left: (left + right) / 2, width: 0, dot: true } : { left, width, dot: false };
}

function isLive(group, now) {
  return group.lastMs != null && now - group.lastMs < HOUR_MS;
}

// The axis moves with the clock, so a bar is placed in place on every render
// rather than through its row's hash: a poll then never rebuilds a row (and
// flashes it) only because "now" advanced.
function placeMark(node, group, axis, now) {
  const mark = lifespanMark(group, axis);
  node.className = `incident-mark ${isLive(group, now) ? "live" : "quiet"}${mark.dot ? " dot" : ""}`;
  node.style.left = `${mark.left.toFixed(2)}%`;
  node.style.width = mark.dot ? "" : `${mark.width.toFixed(2)}%`;
}

function trackNode(group, axis, now, className) {
  const node = el("span", { class: "incident-mark" });
  placeMark(node, group, axis, now);
  const track = el("span", { class: className, "aria-hidden": "true" }, [node]);
  track.incidentMark = node;
  return track;
}

function replaceMarks(container, groups, axis, now) {
  const byKey = new Map(groups.map(group => [group.key, group]));
  for (const node of Array.from(container.children)) {
    const group = byKey.get(node.incidentGroupKey);
    if (group && node.incidentTrack) placeMark(node.incidentTrack.incidentMark, group, axis, now);
  }
}

// syncNodes flashes a node it inserts or replaces, which marks data that
// arrived. A render the operator caused (a click, a filter) only flashes noise.
function settle(container) {
  for (const node of Array.from(container.children)) {
    if (node.classList && typeof node.classList.remove === "function") node.classList.remove("data-new", "data-changed");
  }
}

function axisNode(axis, parts, className) {
  return el("span", { class: className }, axisLabels(axis, parts).map(text => el("span", { text })));
}

// ===== Header, class tiles, denominator and insight chips.

function windowSegmentsNode() {
  const current = getIncidentWindow();
  const values = INCIDENT_WINDOWS.includes(current) ? INCIDENT_WINDOWS : [...INCIDENT_WINDOWS, current];
  const group = el("div", { class: "incident-windows", role: "group", "aria-label": "Time window" });
  for (const value of values) {
    const on = value === current;
    const button = el("button", {
      class: `incident-seg${on ? " on" : ""}`,
      type: "button",
      "aria-pressed": on ? "true" : "false",
      text: value,
    });
    button.dataset.window = value;
    button.addEventListener("click", () => selectWindow(value));
    group.appendChild(button);
  }
  return group;
}

function selectWindow(value) {
  if (value === getIncidentWindow()) return;
  if (DASHBOARD_WINDOWS.includes(value)) {
    windowOverride = null;
    if (value !== getWindow()) {
      // A shared window moves every view; the scope listener refreshes them.
      setWindow(value);
      rerender();
      notifyScopeChange();
      return;
    }
  } else {
    windowOverride = { value, base: getWindow() };
  }
  rerender();
  if (hasCtx(lastCtx, "refreshDiagnostics")) lastCtx.refreshDiagnostics();
}

function headNode() {
  const node = el("div", { class: "incident-head-bar" }, [
    el("p", { class: "incident-intro", text: "Failed audit events, rolled up by signature across runs." }),
    windowSegmentsNode(),
  ]);
  node.dataset.key = "incident-head";
  node.dataset.hash = getIncidentWindow();
  return node;
}

function classTilesNode(payload, window) {
  const categories = payload.failure_categories || {};
  const byClass = payload.incidents_by_class || {};
  const eventsByClass = payload.raw_events_by_class || {};
  const runsByClass = payload.affected_runs_by_class || {};
  const tiles = el("div", { class: "incident-tiles", role: "group", "aria-label": "Incident class" });
  const facts = [];
  for (const key of CLASS_ORDER) {
    const category = categories[key] || {};
    const incidents = asCount(category.incidents ?? byClass[key]);
    const events = asCount(category.raw_events ?? eventsByClass[key]);
    const runs = asCount(category.affected_runs ?? runsByClass[key]);
    facts.push([key, incidents, events, runs]);
    const on = incidentClass === key;
    const tile = el("button", {
      class: `incident-tile ${key}${on ? " on" : ""}`,
      type: "button",
      "aria-pressed": on ? "true" : "false",
      title: `${incidentClassLabel(key, (payload.class_labels || {})[key])}: ${plural(incidents, "incident")} ${windowPhrase(window)}`,
    }, [
      el("span", { class: "incident-tile-label" }, [el("span", { class: "incident-dot", "aria-hidden": "true" }), CLASS_TITLES[key]]),
      el("span", { class: "incident-tile-count" }, [
        el("span", { class: "incident-tile-number", text: number(incidents) }),
        el("span", { text: incidents === 1 ? " incident" : " incidents" }),
      ]),
      el("span", { class: "incident-tile-sub", text: `${plural(events, "event")} · ${plural(runs, "run")}` }),
      key === "diagnostic" ? el("span", { class: "incident-tile-note", text: "excluded from tool rates" }) : null,
    ]);
    tile.dataset.class = key;
    tile.addEventListener("click", () => selectClass(key));
    tiles.appendChild(tile);
  }

  const failed = asCount(payload.raw_failed_events);
  const total = asCount(payload.total_events);
  const share = total > 0 ? ` (${(failed / total * 100).toFixed(1)}%)` : "";
  const children = [
    tiles,
    el("p", {
      class: "incident-denominator",
      text: `${number(payload.incident_count)} incidents ${windowPhrase(window)}: ${number(failed)} failed events across ${number(payload.affected_run_count)} runs, out of ${number(total)} audited events${share}.`,
    }),
  ];
  if (payload.truncated) {
    children.push(el("p", {
      class: "incident-scan-note",
      text: "Scan limit reached — older failed events in this window were not grouped. Narrow the window for a complete count.",
    }));
  }
  const section = el("section", { class: "incident-classes", "aria-label": "Incident classes" }, children);
  section.dataset.key = "incident-classes";
  section.dataset.hash = JSON.stringify([incidentClass, window, facts, failed, total, payload.incident_count, payload.affected_run_count, payload.truncated]);
  return section;
}

function selectClass(key) {
  if (incidentClass === key) return;
  incidentClass = key;
  selectedKey = null;
  rerender();
  if (hasCtx(lastCtx, "refreshDiagnostics")) lastCtx.refreshDiagnostics();
}

function insightsNode(groups, now) {
  const live = groups.filter(group => isLive(group, now)).length;
  const silent = groups.filter(group => !group.message.recorded).length;
  const chips = [];
  if (live > 0 || lastHourOnly) {
    const chip = el("button", {
      class: `incident-insight live${lastHourOnly ? " on" : ""}`,
      type: "button",
      "aria-pressed": lastHourOnly ? "true" : "false",
    }, [el("span", { class: "incident-dot", "aria-hidden": "true" }), `${plural(live, "signature")} failed in the last hour`]);
    chip.dataset.insight = "last-hour";
    chip.addEventListener("click", () => { lastHourOnly = !lastHourOnly; rerender(); });
    chips.push(chip);
  }
  if (silent > 0 || noMessageOnly) {
    const chip = el("button", {
      class: `incident-insight silent${noMessageOnly ? " on" : ""}`,
      type: "button",
      "aria-pressed": noMessageOnly ? "true" : "false",
    }, [el("span", { class: "incident-dot", "aria-hidden": "true" }), `${plural(silent, "signature")} ${silent === 1 ? "records" : "record"} no error message`]);
    chip.dataset.insight = "no-message";
    chip.addEventListener("click", () => {
      noMessageOnly = !noMessageOnly;
      if (noMessageOnly) {
        const first = groups.find(group => !group.message.recorded);
        if (first) selectedKey = first.key;
      }
      rerender();
    });
    chips.push(chip);
  }
  const node = el("div", { class: "incident-insights", role: "group", "aria-label": "Insights" }, chips);
  node.dataset.key = "incident-insights";
  node.dataset.hash = JSON.stringify([live, silent, lastHourOnly, noMessageOnly]);
  return node;
}

// ===== List (master).

function matchesSearch(group, query) {
  if (!query) return true;
  const haystack = group.incidents.flatMap(incident => [
    incident.message, incident.surface, incident.actor, incident.activity_id, incident.incident_id,
    ...incidentRunIds(incident), ...(Array.isArray(incident.task_ids) ? incident.task_ids : []),
  ]).concat(group.message.text).filter(Boolean).join("\n").toLowerCase();
  return haystack.includes(query);
}

function visibleGroups(groups, now) {
  const query = searchQuery.trim().toLowerCase();
  return groups.filter(group => (!lastHourOnly || isLive(group, now))
    && (!noMessageOnly || !group.message.recorded)
    && matchesSearch(group, query));
}

function rowNode(group, axis, now, ctx, cap) {
  const selected = group.key === selectedKey;
  const live = isLive(group, now);
  const track = trackNode(group, axis, now, "incident-track");
  const cadence = group.recurring ? cadenceLabel(group.incidents) : "";
  const lastLabel = group.last ? fmtRelativeValue(ctx, group.last) : "-";
  const row = el("button", {
    class: `incident-row ${group.cls}${selected ? " selected" : ""}`,
    type: "button",
    "aria-pressed": selected ? "true" : "false",
  }, [
    el("span", { class: "incident-main" }, [
      el("span", { class: "incident-title" }, [
        el("span", { class: "incident-headline", text: truncateValue(ctx, group.title, 240) }),
        group.message.recorded ? null : el("span", { class: "incident-flag", text: "no message" }),
      ]),
      el("span", {
        class: "incident-sub",
        text: [group.sample.surface || "unknown surface", actorLabel(group.incidents), cadence].filter(Boolean).join(" · "),
      }),
    ]),
    track,
    el("span", { class: "incident-count", text: countLabel(group), title: countTitle(group, cap) }),
    el("span", { class: `incident-last${live ? " live" : ""}`, text: lastLabel, title: group.last ? `last seen ${formatDateTime(group.last)}` : "" }),
  ]);
  row.dataset.key = `incident-${group.key}`;
  row.dataset.hash = JSON.stringify([selected, live, lastLabel, group.events, group.incidents.length, row.textContent]);
  row.incidentGroupKey = group.key;
  row.incidentTrack = track;
  row.addEventListener("click", () => selectGroup(group.key, { reveal: true }));
  return row;
}

function selectGroup(key, { reveal = false } = {}) {
  selectedKey = key;
  rerender();
  if (!reveal || !layout) return;
  // Stacked on a phone, the panel sits below the list: bring it into view.
  const { master, detail } = layout;
  if (typeof detail.getBoundingClientRect !== "function" || typeof detail.scrollIntoView !== "function") return;
  if (detail.getBoundingClientRect().top >= master.getBoundingClientRect().bottom - 1) {
    detail.scrollIntoView({ block: "start" });
  }
}

function ensureLayout(body) {
  if (layout && body.contains(layout.root)) return layout;
  const heading = el("h3", { class: "incident-list-title" });
  const search = el("input", { class: "incident-search", type: "search" });
  search.id = "incident-search";
  search.placeholder = "Filter by message, surface, actor, run";
  search.value = searchQuery;
  search.addEventListener("input", () => { searchQuery = search.value; rerender(); });
  const searchLabel = el("label", { class: "visually-hidden", text: "Filter incidents" });
  searchLabel.htmlFor = "incident-search";
  const lastHour = el("button", { class: "incident-toggle", type: "button", text: "Last hour only" });
  lastHour.addEventListener("click", () => { lastHourOnly = !lastHourOnly; rerender(); });
  const toolbar = el("div", { class: "incident-toolbar" }, [
    heading,
    el("div", { class: "incident-search-wrap" }, [searchLabel, search]),
    lastHour,
  ]);
  const axisHead = el("div", { class: "incident-axis-head", "aria-hidden": "true" });
  const rows = el("div", { class: "incident-rows", role: "group", "aria-label": "Incident signatures" });
  const footer = el("div", { class: "incident-footer" });
  const master = el("section", { class: "incident-master", "aria-label": "Incident list" }, [toolbar, axisHead, rows, footer]);
  const detail = el("aside", { class: "incident-detail-panel", "aria-label": "Selected incident" });
  const root = el("div", { class: "incident-layout" }, [master, detail]);
  root.dataset.key = "incident-layout";
  root.dataset.hash = "incident-layout";
  layout = { root, heading, search, lastHour, axisHead, rows, footer, master, detail };
  return layout;
}

function emptyNode(text, { clear = false, status = false } = {}) {
  const children = [el("span", { text })];
  if (clear) {
    const button = el("button", { class: "incident-link-button", type: "button", text: "Clear filters" });
    button.addEventListener("click", () => {
      searchQuery = "";
      lastHourOnly = false;
      noMessageOnly = false;
      if (layout) layout.search.value = "";
      rerender();
    });
    children.push(button);
  }
  const node = el("div", { class: "incident-empty", role: status ? "status" : null }, children);
  node.dataset.key = `incident-empty-${text}`;
  node.dataset.hash = text;
  return node;
}

function footerNodes(payload, cap, ctx, window) {
  const label = CLASS_TITLES[incidentClass].toLowerCase();
  const scope = cap.capped
    ? `Rows roll up the newest ${number(cap.shown)} of ${number(cap.matching)} ${label} ${windowPhrase(window)}.`
    : `Rows roll up all ${number(cap.matching || cap.shown)} ${label} ${windowPhrase(window)}.`;
  const nodes = [el("span", { text: `${scope} Sorted by last seen.` })];
  const current = asCount(payload.limit) || INCIDENT_PAGE;
  const next = Math.min(current + INCIDENT_PAGE, INCIDENT_PAGE_MAX);
  if (cap.capped && next > current && hasCtx(ctx, "loadMoreIncidents")) {
    const more = el("button", { class: "incident-link-button", type: "button", text: "Load older", title: `Load up to ${next} incidents` });
    more.addEventListener("click", () => ctx.loadMoreIncidents(next));
    nodes.push(more);
  }
  return nodes;
}

// ===== Detail panel.

function pill(text, className) {
  return el("span", { class: `incident-pill ${className}`, text });
}

function summarySentence(group, ctx) {
  const count = group.runs >= 2 ? `${group.runs} runs` : eventCountLabel(group.events);
  const last = fmtRelativeValue(ctx, group.last);
  const first = group.first ? fmtRelativeValue(ctx, group.first) : last;
  // A burst whose first and last sightings read the same is one moment.
  if (first === last) return `${count}, all ${last} ago.`;
  const cadence = group.recurring ? cadenceLabel(group.incidents) : "";
  return `${count} since ${first} ago${cadence ? `, ${cadence}` : ""}. Last seen ${last} ago.`;
}

function factsNode(group, ctx) {
  const facts = el("dl", { class: "incident-facts" });
  const fact = (label, value, { mono = false, title = "" } = {}) => {
    facts.appendChild(el("div", {}, [
      el("dt", { text: label }),
      el("dd", { class: mono ? "mono" : "", text: value, title }),
    ]));
  };
  const steps = [...new Set(group.incidents.map(incident => incident.activity_id).filter(Boolean))];
  fact("Surface", group.sample.surface || "-", { mono: true });
  fact("Actor", actorLabel(group.incidents), { title: sharedIncidentActor(group.incidents) || "" });
  fact("Step", steps.length ? steps.join(" · ") : "-", { mono: true });
  fact("Count", [eventCountLabel(group.events), group.runs ? plural(group.runs, "run") : "no run recorded"].join(" · "));
  fact("First seen", group.first ? `${fmtRelativeValue(ctx, group.first)} ago` : "-", { title: formatDateTime(group.first) });
  fact("Last seen", group.last ? `${fmtRelativeValue(ctx, group.last)} ago` : "-", { title: formatDateTime(group.last) });
  return facts;
}

function groupEventIds(group) {
  const ids = group.incidents.flatMap(incident => Array.isArray(incident.events) ? incident.events.map(event => event.id) : [])
    .filter(id => Number.isSafeInteger(id) && id > 0);
  return ids.length > 0 && ids.length === group.events ? ids : null;
}

// Exact event IDs when the payload carried every one; otherwise the actor and
// surface narrow the raw Audit view to the same population.
function openAuditEvents(group) {
  const eventIds = groupEventIds(group);
  const sample = group.sample;
  navigateToDrilldown({
    role: eventIds ? null : (sharedIncidentActor(group.incidents) || null),
    tool: eventIds || sample.has_tool_identity === false ? null : (sample.surface || null),
    eventIds,
  });
}

function auditButton(group, text, className) {
  const button = el("button", { class: className, type: "button", text, title: "Every underlying event stays in the raw Audit view" });
  button.addEventListener("click", () => openAuditEvents(group));
  return button;
}

function runLink(runId, workspaceId, ctx) {
  const link = el("a", { class: "mono", text: runId, title: `Open ${runId}` });
  link.href = runHref(runId, { workspace: workspaceId });
  link.addEventListener("click", event => {
    event.stopPropagation();
    if (!plainClick(event)) return;
    event.preventDefault();
    if (hasCtx(ctx, "navigateToRun")) ctx.navigateToRun(runId, workspaceId);
  });
  return link;
}

function taskLink(taskId, workspaceId) {
  const link = el("a", { class: "mono", text: taskId, title: `Open ${taskId}` });
  bindTaskLink(link, taskId, { workspace: workspaceId });
  return link;
}

function joined(nodes) {
  const out = [];
  nodes.forEach((node, index) => {
    if (index) out.push(" · ");
    out.push(node);
  });
  return out;
}

// Each run of the signature keeps its own incident: id, run, tasks, count.
function runsNode(group, ctx) {
  const details = el("details", { class: "incident-disclosure incident-runs" });
  details.open = runsOpen;
  details.addEventListener("toggle", () => { if (details.isConnected !== false) runsOpen = details.open; });
  details.appendChild(el("summary", { text: `Runs (${group.runs || 0} across ${plural(group.incidents.length, "incident")})` }));
  const list = el("ul", { class: "incident-run-list" });
  for (const incident of group.incidents) {
    const workspaceId = getWorkspace();
    const runIds = incidentRunIds(incident);
    const tasks = Array.isArray(incident.task_ids) ? incident.task_ids.filter(Boolean) : [];
    const item = el("li", { class: "incident-run" }, [
      el("span", { class: "incident-run-links" }, runIds.length ? joined(runIds.map(id => runLink(id, workspaceId, ctx))) : ["no run recorded"]),
      el("span", { class: "incident-run-meta", text: `${eventCountLabel(incident.event_count)} · ${fmtRelativeValue(ctx, incident.last_ts)} ago`, title: formatDateTime(incident.last_ts) }),
      tasks.length ? el("span", { class: "incident-run-tasks" }, joined(tasks.map(id => taskLink(id, workspaceId)))) : null,
    ]);
    item.dataset.incident = incident.incident_id || "";
    item.title = `Incident ${incident.incident_id || "-"}`;
    list.appendChild(item);
  }
  details.appendChild(list);
  return details;
}

function evidenceTable(events, ctx) {
  const table = el("table", { class: "incident-evidence" });
  const headRow = el("tr");
  for (const label of ["event", "execution", "time", "status", "actor", "surface", "tool", "run", "task", "message"]) {
    headRow.appendChild(el("th", { text: label }));
  }
  table.appendChild(el("thead", {}, [headRow]));
  const tbody = el("tbody");
  for (const event of events) {
    const workspaceId = event.workspace_id || getWorkspace();
    const tr = el("tr");
    tr.appendChild(el("td", { class: "mono", text: event.id == null ? "-" : `#${event.id}` }));
    tr.appendChild(el("td", { class: "mono", text: event.execution_id || "-" }));
    tr.appendChild(el("td", { text: hasCtx(ctx, "fmtAbsTime") ? ctx.fmtAbsTime(event.ts) : (event.ts || "-") }));
    tr.appendChild(el("td", { text: event.status || "-" }));
    tr.appendChild(el("td", { text: event.actor || "-" }));
    tr.appendChild(el("td", { class: "mono", text: event.surface || "-" }));
    tr.appendChild(el("td", { class: "mono", text: event.tool || "-" }));
    tr.appendChild(el("td", { class: "mono" }, [event.run_id ? runLink(event.run_id, workspaceId, ctx) : "-"]));
    tr.appendChild(el("td", { class: "mono" }, [event.task_id ? taskLink(event.task_id, workspaceId) : "-"]));
    const message = el("td", { class: "stderr", text: truncateValue(ctx, event.message || "", 160) });
    message.title = event.message || "";
    tr.appendChild(message);
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  return table;
}

// The forensic record: why these rows grouped, what they propagated to, and
// every underlying audit row the payload carried.
function evidenceNode(group, ctx) {
  const details = el("details", { class: "incident-disclosure incident-evidence-details" });
  details.open = evidenceOpen;
  details.addEventListener("toggle", () => { if (details.isConnected !== false) evidenceOpen = details.open; });
  const events = group.incidents.flatMap(incidentEvents);
  details.appendChild(el("summary", { text: `Audit evidence (${events.length} of ${eventCountLabel(group.events)} shown)` }));
  const signatures = [...new Set(group.incidents.map(incident => incident.signature).filter(Boolean))];
  details.appendChild(el("dl", { class: "incident-signature" }, [
    el("dt", { text: "Grouping signature" }),
    el("dd", { class: "mono", text: signatures.length ? signatures.join("\n") : "none recorded" }),
  ]));
  const propagation = group.incidents.flatMap(incident => Array.isArray(incident.propagation) ? incident.propagation : []);
  if (propagation.length > 0) {
    const chain = el("div", { class: "incident-propagation" }, [el("div", {
      class: "incident-section-title",
      text: `Propagation from this root (${propagation.length} downstream failures, not independent root causes)`,
    })]);
    for (const link of propagation) {
      chain.appendChild(el("div", { class: "incident-propagation-link" }, [
        el("span", { class: "chain-mark", text: "↳" }),
        el("span", { class: "chain-surface mono", text: link.surface || "-" }),
        el("span", { class: "chain-count", text: eventCountLabel(link.event_count) }),
        el("span", { class: "chain-message", text: truncateValue(ctx, incidentMessageText(link), 140) }),
      ]));
    }
    details.appendChild(chain);
  }
  if (events.length > 0) {
    details.appendChild(el("div", { class: "incident-evidence-wrap" }, [evidenceTable(events, ctx)]));
  }
  return details;
}

function relatedNode(group, groups, ctx) {
  if (!group.sameMessage) return null;
  const related = groups.filter(other => other.key !== group.key && other.sameMessage === group.sameMessage);
  if (related.length === 0) return null;
  const section = el("section", { class: "incident-related", "aria-label": "Other signatures with this message" }, [
    el("h4", { class: "incident-eyebrow", text: "Same message, other signatures" }),
  ]);
  for (const other of related.slice(0, RELATED_MAX)) {
    const button = el("button", { class: "incident-related-item", type: "button" }, [
      el("span", { class: "mono", text: `${other.sample.surface || "unknown surface"} · ${actorLabel(other.incidents)}` }),
      el("span", { class: "incident-related-meta", text: `${countLabel(other)} · ${fmtRelativeValue(ctx, other.last)}` }),
    ]);
    button.addEventListener("click", () => selectGroup(other.key));
    section.appendChild(button);
  }
  if (related.length > RELATED_MAX) {
    const more = related.length - RELATED_MAX;
    section.appendChild(el("p", { class: "incident-related-more", text: `${plural(more, "more signature")} ${more === 1 ? "shares" : "share"} this message.` }));
  }
  return section;
}

function detailNode(group, groups, axis, now, ctx) {
  const live = isLive(group, now);
  const track = trackNode(group, axis, now, "incident-wide-track");
  const message = group.message;
  const messageChildren = [
    el("div", { class: "incident-message-head" }, [
      el("h4", { class: "incident-eyebrow", text: "Message" }),
      makeCopyButton(message.text, { class: "incident-copy", text: "Copy", title: "Copy the full message" }),
    ]),
    el("pre", { class: `incident-full-message${message.recorded ? "" : " missing"}`, text: message.text }),
  ];
  if (message.truncated) {
    messageChildren.push(el("p", { class: "incident-message-note" }, [
      "This message was cut short before it reached the incidents feed. ",
      auditButton(group, "Open the audit events", "incident-link-button"),
      " to read each underlying record.",
    ]));
  }
  const actions = [];
  const runIds = [...new Set(group.incidents.flatMap(incidentRunIds))];
  if (runIds.length === 1 && hasCtx(ctx, "setActiveTab")) {
    const view = el("button", { class: "incident-action", type: "button", text: "View run", title: `Open ${runIds[0]}` });
    view.addEventListener("click", () => ctx.setActiveTab(`runs/${encodeURIComponent(runIds[0])}`));
    actions.push(view);
  } else if (runIds.length > 1) {
    const view = el("button", { class: "incident-action", type: "button", text: `View runs (${runIds.length})` });
    view.addEventListener("click", () => {
      runsOpen = true;
      rerender();
      const runs = layout && layout.detail.querySelector && layout.detail.querySelector(".incident-runs");
      if (runs && typeof runs.scrollIntoView === "function") runs.scrollIntoView({ block: "nearest" });
    });
    actions.push(view);
  }
  actions.push(auditButton(group, "Audit events", "incident-action"));

  const node = el("div", { class: "incident-detail" }, [
    el("div", { class: "incident-pills" }, [
      pill(CLASS_NAMES[group.cls] || incidentClassLabel(group.cls, group.sample.class_label), group.cls),
      pill(live ? "Failed in the last hour" : `Quiet for ${fmtRelativeValue(ctx, group.last)}`, live ? "live" : "quiet"),
    ]),
    el("div", { class: "incident-detail-head" }, [
      el("h3", { class: "incident-detail-title", text: truncateValue(ctx, group.title, 240) }),
      el("p", { class: "incident-sentence", text: summarySentence(group, ctx) }),
    ]),
    el("div", { class: "incident-lifespan" }, [
      track,
      axisNode(axis, 4, "incident-wide-axis"),
    ]),
    factsNode(group, ctx),
    el("section", { class: "incident-message", "aria-label": "Message" }, messageChildren),
    relatedNode(group, groups, ctx),
    group.incidents.some(incident => incidentRunIds(incident).length) ? runsNode(group, ctx) : null,
    evidenceNode(group, ctx),
    el("div", { class: "incident-actions" }, actions),
  ]);
  node.dataset.key = `detail-${group.key}`;
  node.dataset.hash = JSON.stringify([runsOpen, evidenceOpen, live, node.textContent]);
  node.incidentGroupKey = group.key;
  node.incidentTrack = track;
  return node;
}

// ===== Render.

function rerender() {
  if (lastPayload) renderIncidents(lastPayload, lastCtx, { interactive: true });
}

export function renderIncidents(payload, ctx = {}, { interactive = false } = {}) {
  lastPayload = payload || {};
  lastCtx = ctx;
  const body = $("diag-body");
  if (!body) return;
  payload = lastPayload;
  const now = Date.now();
  const window = getIncidentWindow();
  // The server filters by class and window. A payload for another class or
  // window is the previous answer still on screen while the new one loads.
  const pending = (payload.class !== undefined && payload.class !== incidentClass)
    || (typeof payload.window === "string" && payload.window !== window);
  const incidents = pending ? [] : (Array.isArray(payload.incidents) ? payload.incidents : [])
    .filter(incident => (incident.class || "unexpected") === incidentClass);
  const groups = groupIncidents(incidents);
  const visible = visibleGroups(groups, now);
  if (!visible.some(group => group.key === selectedKey)) selectedKey = visible.length ? visible[0].key : null;
  const cap = listCap(payload);
  const axis = incidentAxis(payload, groups, now);

  const count = $("diag-count");
  if (count) {
    count.textContent = `Newest ${asCount(payload.shown_incident_count)} of ${asCount(payload.matching_incident_count)} incidents · window ${payload.window || window}`;
  }

  const view = ensureLayout(body);
  const label = CLASS_TITLES[incidentClass];
  view.heading.textContent = label;
  view.heading.appendChild(el("span", {
    class: "incident-list-count",
    text: pending ? "" : ` ${plural(visible.length, "signature")}${visible.length === groups.length ? "" : ` of ${groups.length}`}`,
  }));
  view.lastHour.className = `incident-toggle${lastHourOnly ? " on" : ""}`;
  view.lastHour.setAttribute("aria-pressed", lastHourOnly ? "true" : "false");
  const axisHead = el("div", { class: "incident-axis-row" }, [
    el("span", { class: "incident-main", text: "Signature" }),
    axisNode(axis, 2, "incident-track incident-axis"),
    el("span", { class: "incident-count", text: "Count" }),
    el("span", { class: "incident-last", text: "Last" }),
  ]);
  axisHead.dataset.key = "incident-axis";
  axisHead.dataset.hash = axisHead.textContent;
  syncNodes(view.axisHead, [axisHead]);

  let rows;
  if (pending) rows = [emptyNode(`Loading ${label.toLowerCase()} ${windowPhrase(window)}…`, { status: true })];
  else if (groups.length === 0) rows = [emptyNode(`No ${label.toLowerCase()} ${windowPhrase(window)}.`)];
  else if (visible.length === 0) rows = [emptyNode("No incident matches these filters.", { clear: true })];
  else rows = visible.map(group => rowNode(group, axis, now, ctx, cap));
  syncNodes(view.rows, rows);
  replaceMarks(view.rows, groups, axis, now);
  const footer = el("div", { class: "incident-footer-row" }, pending ? [] : footerNodes(payload, cap, ctx, window));
  footer.dataset.key = "incident-footer";
  footer.dataset.hash = JSON.stringify([footer.textContent, pending]);
  syncNodes(view.footer, [footer]);

  const selected = visible.find(group => group.key === selectedKey);
  let detail;
  if (selected) {
    detail = detailNode(selected, groups, axis, now, ctx);
  } else {
    detail = el("p", { class: "incident-detail-empty", text: "Select a signature to see its full message, runs and audit events." });
    detail.dataset.key = "detail-empty";
    detail.dataset.hash = "detail-empty";
  }
  syncNodes(view.detail, [detail]);
  replaceMarks(view.detail, groups, axis, now);

  syncNodes(body, [
    headNode(),
    classTilesNode(payload, payload.window || window),
    insightsNode(pending ? [] : groups, now),
    view.root,
  ]);
  if (interactive) [body, view.axisHead, view.rows, view.footer, view.detail].forEach(settle);
}

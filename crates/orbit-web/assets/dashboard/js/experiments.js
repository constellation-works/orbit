// Orbit dashboard Analytics › Experiments: the workspace's experiments with
// each arm's enrollment toward its minimum sample, and one experiment's
// readout: arm cards, a forest plot of every metric's difference from
// control, the primary metric's cumulative per-arm series, the
// per-complexity table, crew mix, health warnings and the event timeline.
// An operator concludes a running or paused experiment from this page, and
// creates, edits and starts drafts through experiment-form.js.
// Read from /api/diagnostics/experiments[/{id}[/readout]]; conclude posts
// to /api/diagnostics/experiments/{id}/conclude.
// Pure vanilla JS, split into ES modules with no build step; every node is
// built with DOM calls, never markup strings.
//
// `#analytics/experiments` is the list and `#analytics/experiments/<id>` one
// experiment. Experiments are per workspace; the aggregate view shows the
// usual placeholder.

import {
  el as makeEl, syncNodes, fetchJson, postJson, requestPanel, panelCanRender, getWorkspaceRevision,
  onWorkspaceChange, renderPanelPlaceholder, isAggregateView, formatDateTime, formatDate, formatClock,
  infoHint, scopedHashHref,
} from './common.js';
import { setActiveTab } from './router.js';
import { svg, linePath } from './sparkline.js';
import { draftActions, draftStartRenderState, lockedNote, syncExperimentAuthoring } from './experiment-form.js';

const $ = (id) => document.getElementById(id);

// `el` from common.js, also applying the table and link attributes it leaves out.
function el(tag, opts = {}, children = []) {
  const node = makeEl(tag, opts, children);
  for (const name of ["scope", "tabindex", "href", "datetime"]) {
    if (opts[name] != null) node.setAttribute(name, String(opts[name]));
  }
  return node;
}

const BODY = "experiments-body";
const COUNT = "experiments-count";
const ENDPOINT = "/api/diagnostics/experiments";
// Arms take the shared categorical palette in definition order, so an arm
// keeps its color on every chart of the page; past seven they fold into one.
const ARM_COLORS = 7;
const STATUS_ORDER = ["running", "paused", "draft", "concluded", "abandoned"];
const WARNING_LABELS = {
  sample_ratio: "Sample ratio",
  drift: "Drift",
  undelivered: "Treatment not delivered",
  lagging: "Open outcomes",
  primary_unmeasured: "Primary unmeasured",
  first_edit_incomplete: "First-edit coverage incomplete",
  tool_trace_incomplete: "Tool trace truncated",
  explorer_trace_incomplete: "Explorer uptake unconfirmed",
  crew_mismatch: "Crew outside arm pool",
  crew_mismatch_unavailable: "Crew admission pool unavailable",
  forced: "Forced assignments",
  explicit_crew: "Explicit-crew exclusions",
};
const GUARDRAIL_LABELS = { ok: "ok", breached: "breached", straddles: "straddles", unknown: "unknown" };
const EVENT_LABELS = {
  created: "Created", updated: "Definition edited", cloned: "Cloned", started: "Started",
  paused: "Paused", resumed: "Resumed", reweighted: "Reweighted", arm_retired: "Arm retired",
  concluded: "Concluded", abandoned: "Abandoned",
};

let selectedId = null;
let lastPayload = null;
let requestScope = null;
let draft = null;

/// The experiment on screen, or null for the list.
export function getExperimentId() {
  return selectedId;
}

export function setExperimentId(id) {
  const next = id ? String(id) : null;
  if (next !== selectedId) draft = null;
  selectedId = next;
}

/// The hash route of the view on screen, without the leading `#`.
export function experimentsRoute(id = selectedId) {
  return id ? `analytics/experiments/${encodeURIComponent(id)}` : "analytics/experiments";
}

function clearExperiments() {
  lastPayload = null;
  const count = $(COUNT);
  if (count) count.textContent = "—";
}

export function placeholdExperimentsAggregate() {
  clearExperiments();
  syncExperimentAuthoring(null);
  renderPanelPlaceholder(BODY);
}

onWorkspaceChange(() => {
  requestScope = null;
  draft = null;
  clearExperiments();
});

// An unknown id answers 400 or 404 naming it; the detail shows that as its
// own state instead of a blank panel.
async function readDetail(id) {
  const base = `${ENDPOINT}/${encodeURIComponent(id)}`;
  try {
    const [experiment, readout] = await Promise.all([fetchJson(base), fetchJson(`${base}/readout`)]);
    return { kind: "detail", id, experiment, readout };
  } catch (error) {
    if (error && (error.status === 400 || error.status === 404) && !error.routeMissing) {
      return { kind: "missing", id, message: error.message };
    }
    throw error;
  }
}

export function fetchAndRenderExperiments() {
  if (isAggregateView()) {
    placeholdExperimentsAggregate();
    return Promise.resolve();
  }
  const revision = getWorkspaceRevision();
  const id = selectedId;
  const scope = `${revision}:${id || ""}`;
  if (requestScope !== scope) {
    requestScope = scope;
    clearExperiments();
  }
  const request = id
    ? () => readDetail(id)
    : () => fetchJson(ENDPOINT).then((body) => ({
      kind: "list", experiments: body.experiments || [], authoring: body.authoring || null,
    }));
  return requestPanel(BODY, `experiments:${id || ""}`, request, (payload) => {
    if (getWorkspaceRevision() !== revision || selectedId !== id) return;
    lastPayload = payload;
    renderExperiments();
  }, COUNT);
}

export function renderExperiments() {
  const body = $(BODY);
  if (!body) return;
  if (isAggregateView()) {
    placeholdExperimentsAggregate();
    return;
  }
  if (!lastPayload || !panelCanRender(BODY)) return;
  const count = $(COUNT);
  const authoring = lastPayload.authoring || lastPayload.experiment?.authoring || null;
  syncExperimentAuthoring(authoring, fetchAndRenderExperiments, renderDraftStartState);
  let nodes;
  if (lastPayload.kind === "list") {
    const total = lastPayload.experiments.length;
    if (count) count.textContent = `${total} experiment${total === 1 ? "" : "s"}`;
    nodes = [listView(lastPayload.experiments, authoring)];
  } else if (lastPayload.kind === "missing") {
    if (count) count.textContent = "not found";
    nodes = [missingView(lastPayload)];
  } else {
    const analysed = lastPayload.readout?.enrollment?.analysed ?? 0;
    if (count) count.textContent = `${analysed} task${analysed === 1 ? "" : "s"} analysed`;
    nodes = [detailView(lastPayload.experiment, lastPayload.readout)];
  }
  const root = el("div", { class: "ex-view" }, nodes);
  root.dataset.key = `experiments-${lastPayload.kind}`;
  root.dataset.hash = JSON.stringify([lastPayload, draft, draftStartRenderState()]);
  syncNodes(body, [root]);
}

function renderDraftStartState(experimentId) {
  renderExperiments();
  const host = Array.from($(BODY)?.querySelectorAll(".exf-draft-actions") || [])
    .find((node) => node.dataset.key === `draft-actions-${experimentId}`);
  host?.querySelector("button.exf-confirm-start")?.focus();
}

// ---- links ------------------------------------------------------------------

/// An in-dashboard link to `id` (or the list): a plain click routes in place,
/// while the href keeps host, workspace and window for a new tab or a copy.
function experimentLink(id, children, className) {
  const route = experimentsRoute(id);
  const link = el("a", { class: className, href: scopedHashHref(`#${route}`) }, children);
  link.addEventListener("click", (event) => {
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    setExperimentId(id);
    setActiveTab(route);
  });
  return link;
}

// ---- formatting -------------------------------------------------------------

function num(value) {
  const n = Number(value);
  return value == null || !Number.isFinite(n) ? null : n;
}

function trim(n, digits) {
  return Number(n.toFixed(digits)).toString();
}

function compact(n) {
  const abs = Math.abs(n);
  if (abs >= 1e9) return `${trim(n / 1e9, 1)}B`;
  if (abs >= 1e6) return `${trim(n / 1e6, 1)}M`;
  if (abs >= 1e3) return `${trim(n / 1e3, abs >= 1e4 ? 0 : 1)}k`;
  if (abs > 0 && abs < 0.1) return Number(n.toPrecision(2)).toString();
  return trim(n, abs >= 100 ? 0 : abs >= 10 ? 1 : 2);
}

/// A metric value in its unit; `diff` prints a signed difference.
function fmtValue(value, unit, { diff = false } = {}) {
  const n = num(value);
  if (n == null) return "—";
  const sign = diff && n > 0 ? "+" : diff && n < 0 ? "−" : "";
  const abs = diff ? Math.abs(n) : n;
  switch (unit) {
    case "rate": return diff ? `${sign}${trim(abs * 100, 1)}pt` : `${trim(abs * 100, abs >= 0.1 ? 0 : 1)}%`;
    case "usd": return `${sign}$${abs >= 100 ? Math.round(abs).toLocaleString() : abs.toFixed(2)}`;
    case "hours": return abs >= 48 ? `${sign}${trim(abs / 24, 1)}d` : `${sign}${trim(abs, 1)}h`;
    case "minutes": return abs >= 120 ? `${sign}${trim(abs / 60, 1)}h` : `${sign}${trim(abs, 1)}m`;
    case "normalized_tokens": return `${sign}${compact(abs)}`;
    default: return `${sign}${compact(abs)}`;
  }
}

function fmtInterval(interval, unit, opts) {
  if (!Array.isArray(interval) || interval.length !== 2) return null;
  return `${fmtValue(interval[0], unit, opts)} to ${fmtValue(interval[1], unit, opts)}`;
}

function metricLabel(name) {
  return String(name || "").replace(/_/g, " ");
}

function unitLabel(unit) {
  return { normalized_tokens: "normalized tokens", usd: "USD" }[unit] || unit;
}

function plural(n, word) {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

function armColor(index) {
  return index < ARM_COLORS ? `mx-s-c${index + 1}` : "mx-s-crew-other";
}

function armIndex(readout) {
  const index = new Map();
  (readout.arms || []).forEach((arm, i) => index.set(arm.name, i));
  return (name) => index.get(name) ?? ARM_COLORS;
}

function swatch(className, extra = "") {
  return el("span", { class: `mx-swatch ${className}${extra}`, "aria-hidden": "true" });
}

function badge(text, className) {
  return el("span", { class: `ex-badge ${className}`, text });
}

function card(title, children, { meta = null, className = "" } = {}) {
  return el("section", { class: `mx-card ex-card ${className}`.trim(), "aria-label": title }, [
    el("div", { class: "mx-card-head" }, [
      el("h2", { class: "mx-card-title", text: title }),
      meta ? el("span", { class: "mx-card-meta", text: meta }) : null,
    ]),
    ...children,
  ]);
}

function empty(title, detail) {
  return el("div", { class: "mx-empty ex-empty" }, [
    el("div", { class: "mx-empty-title", text: title }),
    detail ? el("div", { class: "mx-empty-detail", text: detail }) : null,
  ]);
}

// ---- list ---------------------------------------------------------------------

function listView(experiments, authoring) {
  if (!experiments.length) {
    return card("Experiments", [empty(
      "No experiments in this workspace",
      authoring?.create?.authorized
        ? "Define one with New experiment or orbit experiment create; it appears here once created."
        : "Define one with orbit experiment create; it appears here once created.",
    )]);
  }
  const rank = (status) => {
    const index = STATUS_ORDER.indexOf(status);
    return index < 0 ? STATUS_ORDER.length : index;
  };
  const sorted = [...experiments].sort((a, b) =>
    rank(a.status) - rank(b.status) || String(b.created_at).localeCompare(String(a.created_at)));
  return el("div", { class: "ex-list", role: "list", "aria-label": "Experiments" },
    sorted.map((experiment) => listItem(experiment, authoring)));
}

function listItem(experiment, authoring) {
  const min = num(experiment.min_sample);
  const arms = experiment.arms || [];
  const enrollment = new Map((experiment.enrollment || []).map((row) => [row.arm, row]));
  const meta = [
    `segment ${experiment.segment ?? 0}`,
    experiment.primary_metric ? `primary ${metricLabel(experiment.primary_metric)}` : "no primary metric",
    min != null ? `min ${min} per arm` : "no minimum sample",
    experiment.max_days != null ? `max ${experiment.max_days} days` : null,
  ].filter(Boolean).join(" · ");
  const rows = arms.map((arm, index) => {
    const row = enrollment.get(arm.name) || { enrolled: 0, segment_enrolled: 0 };
    const share = min ? Math.min(1, row.enrolled / min) : 0;
    const label = min ? `${row.enrolled} / ${min}` : `${row.enrolled} enrolled`;
    const bar = el("span", { class: "ex-progress", role: "img", "aria-label": `${arm.name}: ${label}` }, [
      el("span", { class: `ex-progress-fill ${armColor(index)}`, style: { width: `${(share * 100).toFixed(1)}%` } }),
    ]);
    return el("div", { class: "ex-arm-progress", role: "listitem" }, [
      el("span", { class: "ex-arm-name" }, [
        swatch(armColor(index)),
        el("span", { text: arm.name }),
        arm.control ? el("span", { class: "mx-sc-tag", text: "control" }) : null,
        arm.retired_segment != null ? el("span", { class: "mx-sc-tag", text: `retired at ${arm.retired_segment}` }) : null,
      ]),
      bar,
      el("span", {
        class: "ex-progress-text",
        text: label,
        title: `${row.segment_enrolled} enrolled in segment ${experiment.segment ?? 0}`,
      }),
    ]);
  });
  const extras = [];
  if (experiment.forced) extras.push(`${plural(experiment.forced, "forced assignment")} left out`);
  if (experiment.excluded) extras.push(`${experiment.excluded} excluded for a surface conflict`);
  if (experiment.concluded_arm) extras.push(`concluded with ${experiment.concluded_arm}`);
  return el("article", { class: "mx-card ex-item", role: "listitem" }, [
    el("div", { class: "ex-item-head" }, [
      experimentLink(experiment.id, [experiment.name || experiment.id], "ex-item-name"),
      el("span", { class: "ex-badges" }, [
        badge(experiment.status, `ex-status-${experiment.status}`),
        badge(experiment.mode, `ex-mode-${experiment.mode}`),
      ]),
    ]),
    experiment.hypothesis ? el("p", { class: "ex-hypothesis", text: experiment.hypothesis }) : null,
    el("div", { class: "ex-item-meta", text: meta }),
    el("div", { class: "ex-arms-progress", role: "list", "aria-label": "Enrollment per arm" }, rows),
    extras.length ? el("div", { class: "ex-item-meta", text: extras.join(" · ") }) : null,
    draftActions(experiment, authoring),
  ]);
}

function missingView(payload) {
  return el("section", { class: "mx-card ex-card ex-missing", role: "alert" }, [
    el("div", { class: "mx-empty ex-empty" }, [
      el("div", { class: "mx-empty-title", text: "Experiment not found" }),
      el("div", { class: "mx-empty-detail", text: payload.message || `No experiment ${payload.id} in this workspace.` }),
      experimentLink(null, ["Back to all experiments"], "ex-back"),
    ]),
  ]);
}

// ---- detail -------------------------------------------------------------------

function detailView(experiment, readout) {
  const color = armIndex(readout);
  const primary = (readout.metrics || []).find((metric) => metric.metric === readout.experiment?.primary_metric) || null;
  return el("div", { class: "ex-detail" }, [
    headerCard(experiment, readout),
    warningsCard(readout),
    armCards(experiment, readout, primary, color),
    el("div", { class: "mx-row" }, [
      forestCard(readout, color),
      cumulativeCard(readout, primary, color),
    ]),
    el("div", { class: "mx-row" }, [
      complexityCard(primary, color),
      crewCard(readout, color),
    ]),
    concludeCard(experiment),
    timelineCard(experiment),
  ]);
}

function verdictBadge(verdict) {
  const state = verdict?.state || "collecting";
  return badge(verdict?.label || state, `ex-verdict ex-verdict-${state}`);
}

function headerCard(experiment, readout) {
  const info = readout.experiment || {};
  const facts = [
    ["Status", experiment.status],
    ["Mode", experiment.mode === "shadow" ? "shadow (assigns, applies nothing)" : experiment.mode],
    ["Segment", String(experiment.segment ?? 0)],
    ["Primary metric", info.primary_metric ? metricLabel(info.primary_metric) : "—"],
    ["Minimum sample", info.min_sample != null ? `${info.min_sample} per arm` : "—"],
    ["Started", info.started_at ? formatDateTime(info.started_at) : "not started"],
    ["Horizon", info.horizon_at ? formatDateTime(info.horizon_at) : info.max_days != null ? `${info.max_days} days after start` : "—"],
    ["As of", formatDateTime(readout.as_of)],
  ];
  if (experiment.concluded_arm) facts.splice(2, 0, ["Concluded with", experiment.concluded_arm]);
  else if (experiment.status === "concluded") facts.splice(2, 0, ["Concluded with", "no winner"]);
  const reasons = readout.verdict?.reasons || [];
  const promotion = promotionTaskId(experiment);
  return el("section", { class: "mx-card ex-card ex-head", "aria-label": "Experiment" }, [
    experimentLink(null, ["All experiments"], "ex-back"),
    el("div", { class: "ex-item-head" }, [
      el("h2", { class: "ex-title", text: experiment.name || experiment.id }),
      el("span", { class: "ex-badges" }, [
        badge(experiment.status, `ex-status-${experiment.status}`),
        badge(experiment.mode, `ex-mode-${experiment.mode}`),
        verdictBadge(readout.verdict),
      ]),
    ]),
    experiment.hypothesis ? el("p", { class: "ex-hypothesis", text: experiment.hypothesis }) : null,
    el("dl", { class: "ex-facts" }, facts.map(([label, value]) =>
      el("div", { class: "ex-fact" }, [el("dt", { text: label }), el("dd", { text: value })]))),
    reasons.length ? el("p", { class: "mx-blurb ex-reasons", text: `Verdict: ${reasons.join("; ")}.` }) : null,
    promotion ? el("p", { class: "ex-promotion" }, [
      el("span", { text: "Promotion task " }),
      taskLink(promotion),
    ]) : null,
    draftActions(experiment, experiment.authoring),
    lockedNote(experiment, experiment.authoring),
  ]);
}

function promotionTaskId(experiment) {
  const event = (experiment.events || []).find((item) => item.kind === "concluded"
    && item.detail && typeof item.detail.promotion_task === "string" && item.detail.promotion_task);
  return event ? event.detail.promotion_task : null;
}

function taskLink(id) {
  const link = el("a", {
    class: "ex-task-link",
    href: scopedHashHref(`#tasks/${encodeURIComponent(id)}`),
    text: id,
  });
  link.addEventListener("click", (event) => {
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    window.location.hash = `#tasks/${encodeURIComponent(id)}`;
  });
  return link;
}

function concludeCard(experiment) {
  const open = experiment.conclude_authorized
    && (experiment.status === "running" || experiment.status === "paused");
  if (!open) return null;
  if (!draft) draft = { arm: "", reason: "", pending: false, status: "" };
  const state = draft;
  const select = el("select", { class: "ex-conclude-control", "aria-label": "Winning arm" });
  const none = el("option", { text: "No winner" });
  none.value = "";
  select.append(none);
  for (const arm of (experiment.arms || []).filter((item) => item.retired_segment == null)) {
    const option = el("option", { text: arm.control ? `${arm.name} (control)` : arm.name });
    option.value = arm.name;
    select.append(option);
  }
  const reason = el("textarea", { class: "ex-conclude-control", "aria-label": "Reason" });
  select.value = state.arm;
  select.addEventListener("change", () => { state.arm = select.value; });
  reason.rows = 3;
  reason.value = state.reason;
  reason.addEventListener("input", () => { state.reason = reason.value; });
  const status = el("p", { class: "ex-conclude-status", role: "status" });
  const button = el("button", { class: "ex-conclude-submit", type: "button", text: "Conclude experiment" });
  status.textContent = state.status;
  button.disabled = state.pending;
  button.addEventListener("click", () => submitConclude(experiment, select, reason, button, status, state));
  return el("section", { class: "mx-card ex-card ex-conclude", "aria-label": "Conclude experiment" }, [
    el("div", { class: "mx-card-head" }, [
      el("h2", { class: "mx-card-title", text: "Conclude" }),
    ]),
    el("p", { class: "mx-blurb", text: "Choosing an arm files one proposed task to make that arm's overlay the default. No winner files nothing. This does not change configuration or repository files." }),
    el("div", { class: "ex-conclude-fields" }, [
      el("label", { class: "ex-conclude-field" }, [el("span", { text: "Winning arm" }), select]),
      el("label", { class: "ex-conclude-field" }, [el("span", { text: "Reason" }), reason]),
    ]),
    el("div", { class: "ex-conclude-actions" }, [button]),
    status,
  ]);
}

async function submitConclude(experiment, select, reason, button, status, state) {
  const why = reason.value.trim();
  if (!why) {
    state.status = status.textContent = "A reason is required.";
    return;
  }
  state.arm = select.value;
  state.reason = reason.value;
  state.pending = button.disabled = true;
  state.status = status.textContent = "Concluding…";
  try {
    await postJson(`${ENDPOINT}/${encodeURIComponent(experiment.id)}/conclude`, {
      arm: select.value.trim() || null,
      reason: why,
    });
    if (draft !== state) return;
    state.pending = false;
    await fetchAndRenderExperiments();
  } catch (error) {
    if (draft !== state) return;
    state.status = error && error.message ? error.message : "Conclude failed.";
    state.pending = false;
    renderExperiments();
  }
}

function warningsCard(readout) {
  const warnings = [...(readout.warnings || [])];
  const forced = readout.enrollment?.forced_excluded || 0;
  if (forced) {
    warnings.push({
      kind: "forced",
      message: `${plural(forced, "forced assignment")} ${forced === 1 ? "is" : "are"} left out of every estimate`,
    });
  }
  const explicitCrew = readout.enrollment?.explicit_crew_excluded || 0;
  if (explicitCrew) {
    warnings.push({
      kind: "explicit_crew",
      message: `${plural(explicitCrew, "task")} with an explicit crew ${explicitCrew === 1 ? "was" : "were"} excluded, since no crew arm would have changed ${explicitCrew === 1 ? "its" : "their"} crew`,
    });
  }
  const interactive = readout.enrollment?.interactive_excluded || 0;
  if (interactive) {
    warnings.push({
      kind: "interactive",
      message: `${plural(interactive, "task")} worked in an interactive session ${interactive === 1 ? "was" : "were"} excluded, since no job applied ${interactive === 1 ? "its" : "their"} arm`,
    });
  }
  const lagging = readout.lagging || {};
  const meta = `${lagging.open_tasks ?? 0} in flight · ${lagging.open_regression_windows ?? 0} in the ${lagging.regression_window_days ?? 14}-day regression window`;
  if (!warnings.length) {
    return card("Health", [el("p", { class: "ex-ok", text: "No warnings: arm counts match the weights, no drift, no open outcomes." })], { meta });
  }
  return card("Health", [
    el("ul", { class: "ex-warnings" }, warnings.map((warning) =>
      el("li", { class: `ex-warning ex-warning-${warning.kind}` }, [
        el("span", { class: "ex-warning-icon", "aria-hidden": "true", text: "!" }),
        el("span", { class: "ex-warning-kind", text: WARNING_LABELS[warning.kind] || warning.kind }),
        el("span", { class: "ex-warning-text", text: warning.message }),
      ]))),
  ], { meta, className: "ex-health" });
}

// The operator's conclusion names the winner, even when the readout withholds
// its own verdict (a sample-ratio mismatch); otherwise the readout's verdict.
function armVerdict(arm, verdict, concludedArm) {
  if (concludedArm) return concludedArm === arm.name ? "winner (concluded)" : arm.control ? "control" : "not chosen";
  if (arm.control) return "control";
  const state = verdict?.state || "collecting";
  if (state === "winner") return verdict.arm === arm.name ? "winner" : "not the winner";
  return verdict?.label || state;
}

function armCards(experiment, readout, primary, color) {
  const arms = readout.arms || [];
  const cost = (readout.metrics || []).find((metric) => metric.metric === "cost_usd");
  const guardrails = readout.guardrails || [];
  const unit = primary?.unit;
  const cards = arms.map((arm) => {
    const estimate = primary?.arms?.find((row) => row.arm === arm.name);
    const perProtocol = readout.per_protocol?.metric?.arms?.find((row) => row.arm === arm.name);
    const delivery = arm.delivery;
    const coverage = cost?.arms?.find((row) => row.arm === arm.name)?.cost_coverage;
    const interval = fmtInterval(estimate?.mean_interval, unit);
    const state = armVerdict(arm, readout.verdict, experiment.concluded_arm);
    const rails = guardrails.map((rail) => {
      const row = rail.arms?.find((entry) => entry.arm === arm.name);
      if (!row) return null;
      return el("li", { class: `ex-rail ex-rail-${row.status}` }, [
        el("span", { text: `${metricLabel(rail.metric)} ${rail.threshold}` }),
        el("span", { class: "ex-rail-status", text: GUARDRAIL_LABELS[row.status] || row.status }),
      ]);
    }).filter(Boolean);
    return el("div", { class: "mx-sc ex-arm", role: "group", "aria-label": `Arm ${arm.name}` }, [
      el("span", { class: "mx-sc-label" }, [
        swatch(armColor(color(arm.name))),
        el("span", { class: "ex-arm-title", text: arm.name }),
        el("span", { class: "mx-sc-tag", text: `weight ${arm.weight}` }),
        arm.drifted ? el("span", { class: "mx-sc-tag ex-warn-text", text: "drifted" }) : null,
      ]),
      el("span", { class: "mx-sc-value", text: estimate ? fmtValue(estimate.mean, unit) : "—" }),
      el("span", { class: "mx-sc-foot", text: interval ? `95% ${interval}` : "no interval yet" }),
      el("span", { class: "mx-sc-sub ex-arm-n" }, [
        `n ${arm.n}`,
        estimate ? ` · ${estimate.n} settled` : "",
        estimate?.open ? ` · ${estimate.open} open` : "",
      ]),
      el("span", {
        class: "mx-sc-foot",
        text: `Explorer: ${arm.tasks_with_explorer_call ?? 0} of ${arm.n ?? 0} tasks`
          + (arm.tasks_explorer_unobserved ? ` (+${arm.tasks_explorer_unobserved} unobserved)` : ""),
        title: "Analysed tasks with at least one explorer-delegation call during implement, at or after assignment. A task counts once. Unobserved tasks recorded no call, but an implement invocation's output outgrew the capture bound and lost its early tool calls, so a call there cannot be ruled out.",
      }),
      delivery ? el("span", {
        class: `mx-sc-foot${delivery.rate != null && delivery.rate < (readout.experiment?.min_delivered_rate ?? 0) ? " ex-warn-text" : ""}`,
        text: `Delivered: ${delivery.delivered} of ${delivery.assigned - delivery.pending} runs`
          + (delivery.rate != null ? ` (${Math.round(delivery.rate * 100)}%)` : "")
          + (delivery.pending ? ` · ${delivery.pending} pending` : ""),
        title: `Settled tasks whose ${delivery.roles.join(", ")} step received this arm's overlay and finished successfully. Below ${Math.round((readout.experiment?.min_delivered_rate ?? 0) * 100)}% the verdict is withheld. Pending tasks are in flight and not yet delivered.`,
      }) : null,
      readout.per_protocol ? el("span", {
        class: "mx-sc-foot",
        text: `Per-protocol (delivered runs only): ${perProtocol?.n ? fmtValue(perProtocol.mean, unit) : "—"} · n ${perProtocol?.n ?? 0}`,
        title: "The primary metric over delivered runs only: each checked treatment arm keeps its delivered tasks, other arms all theirs. The value above is intention-to-treat, over every assigned run.",
      }) : null,
      coverage ? el("span", {
        class: "mx-sc-foot ex-arm-cost",
        text: `Cost: ${coverage.reported} reported · ${coverage.estimated} estimated · ${coverage.unmeasured} unmeasured`,
        title: "Settled tasks: reported means every invocation has a provider cost; estimated uses token prices for at least one invocation. Tasks missing any cost are unmeasured and excluded from cost comparisons. Open tasks are separate.",
      }) : null,
      rails.length ? el("ul", { class: "ex-rails", "aria-label": "Guardrails" }, rails) : null,
      el("span", { class: `ex-arm-verdict ex-arm-verdict-${state.replace(/\W+/g, "-").replace(/-$/, "")}`, text: state }),
    ]);
  });
  const title = primary ? `Arms · ${metricLabel(primary.metric)}` : "Arms";
  const meta = primary ? `${primary.kind === "rate" ? "rate" : "mean"}, ${primary.better} is better · ${unitLabel(primary.unit)}` : "no primary metric";
  return card(title, [el("div", { class: "mx-series ex-arms" }, cards)], { meta });
}

// A forest plot: one row per metric and treatment arm, the dot the
// difference from control and the bar its 95% interval. Metrics have
// different units, so each metric's rows share their own scale, symmetric
// around zero, and print their numbers; no axis spans two metrics.
// Every axis points the same way: better is to the right. A lower-is-better
// metric is drawn mirrored, while its printed numbers stay the signed arm
// minus control. The dot and bar take the verdict's colour, not the arm's.
const FOREST_VERDICTS = {
  better: "better at 95%",
  worse: "worse at 95%",
  unclear: "interval includes zero",
};

// `oriented` is the interval with better as positive.
function forestVerdict(oriented) {
  if (!oriented) return "unclear";
  if (oriented[0] > 0) return "better";
  if (oriented[1] < 0) return "worse";
  return "unclear";
}

function forestCard(readout, color) {
  const metrics = (readout.metrics || []).filter((metric) => metric.measured
    && (metric.differences || []).some((diff) => num(diff.overall?.estimate) != null));
  const primaryName = readout.experiment?.primary_metric;
  const guardNames = new Set((readout.guardrails || []).map((rail) => rail.metric));
  metrics.sort((a, b) => (b.metric === primaryName) - (a.metric === primaryName)
    || guardNames.has(b.metric) - guardNames.has(a.metric));
  const unmeasured = (readout.metrics || []).filter((metric) => !metric.measured).map((metric) => `${metricLabel(metric.metric)}${metric.exploratory ? " (exploratory)" : ""}`);
  if (!metrics.length) {
    return card("Difference from control", [empty("No differences yet", "Every metric needs settled tasks in control and a treatment arm.")], { className: "mx-wide" });
  }
  const groups = metrics.map((metric) => {
    const bounds = metric.differences.flatMap((diff) => [diff.overall?.estimate, ...(diff.overall?.interval || [])])
      .map(num).filter((n) => n != null).map(Math.abs);
    const extent = Math.max(...bounds, 0) * 1.1 || 1;
    const sign = metric.better === "lower" ? -1 : 1;
    const x = (value) => `${(((sign * value + extent) / (2 * extent)) * 100).toFixed(2)}%`;
    const direction = metric.better === "lower" ? "lower is better" : "higher is better";
    const tags = [
      metric.metric === primaryName ? "primary" : null,
      metric.exploratory ? "exploratory" : null,
      guardNames.has(metric.metric) ? "guardrail" : null,
    ].filter(Boolean);
    const rows = metric.differences.map((diff) => {
      const estimate = num(diff.overall?.estimate);
      const interval = diff.overall?.interval;
      const colorClass = armColor(color(diff.arm));
      const text = estimate == null ? "—" : fmtValue(estimate, metric.unit, { diff: true });
      const range = fmtInterval(interval, metric.unit, { diff: true });
      const bounds = Array.isArray(interval) ? interval.map(num) : null;
      const oriented = bounds && bounds.every((n) => n != null)
        ? bounds.map((n) => sign * n).sort((a, b) => a - b) : null;
      const verdict = forestVerdict(oriented);
      const plot = el("span", { class: "ex-forest-plot", "aria-hidden": "true" }, [
        el("span", { class: "ex-forest-zero", style: { left: "50%" } }),
        oriented ? el("span", {
          class: `ex-forest-ci ex-forest-${verdict}`,
          style: { left: x(sign * oriented[0]), width: `${(((oriented[1] - oriented[0]) / (2 * extent)) * 100).toFixed(2)}%` },
        }) : null,
        estimate != null ? el("span", { class: `ex-forest-dot ex-forest-${verdict}`, style: { left: x(estimate) } }) : null,
      ]);
      return el("div", {
        class: "ex-forest-row",
        role: "listitem",
        title: `${metricLabel(metric.metric)} (${direction}), ${diff.arm} minus control: ${text}${range ? ` (95% ${range})` : ""}; ${FOREST_VERDICTS[verdict]}`,
      }, [
        el("span", { class: "ex-forest-arm" }, [swatch(colorClass), el("span", { text: diff.arm })]),
        plot,
        el("span", { class: "ex-forest-value" }, [
          el("span", { class: "ex-forest-est", text }),
          range ? el("span", { class: "ex-forest-range", text: range }) : null,
        ]),
      ]);
    });
    return el("div", { class: "ex-forest-group" }, [
      el("div", { class: "ex-forest-metric" }, [
        el("span", { class: "ex-forest-name", text: metricLabel(metric.metric) }),
        ...tags.map((tag) => el("span", { class: "mx-sc-tag", text: tag })),
      ]),
      el("div", { class: "ex-forest-rows", role: "list" }, rows),
    ]);
  });
  return card("Difference from control", [
    el("p", { class: "mx-blurb" }, [
      "Arm minus control, post-stratified by complexity, with 95% intervals. Each metric has its own scale around zero, and better is always to the right. ",
      infoHint("Lower-is-better metrics such as cost and tokens are drawn mirrored so that better points right; their numbers stay the signed arm minus control, so a cost cut prints negative. Green is better and red worse at 95%; grey is a bar that crosses the zero line, a difference the sample cannot tell from none. The 95% intervals are post-stratified bootstrap intervals of the difference for mean and rate metrics alike. Wilson intervals apply to a single arm's rate estimate."),
    ]),
    el("div", { class: "ex-forest" }, [
      el("div", { class: "ex-forest-axis", "aria-hidden": "true" }, [
        el("span"),
        el("span", { class: "ex-forest-axis-plot" }, [
          el("span", { text: "← worse" }),
          el("span", { class: "ex-forest-axis-better", text: "better →" }),
        ]),
        el("span"),
      ]),
      ...groups,
    ]),
    unmeasured.length ? el("p", { class: "ex-note", text: `Not measured yet: ${unmeasured.join(", ")}.` }) : null,
  ], { className: "mx-wide" });
}

// The primary metric's per-arm cumulative mean at each bucket end of the
// shared trend ladder, over settled tasks assigned by then.

// Axis text follows the dashboard clock at the bucket's own precision. An
// hour or 6 hours can fall on the same clock time on a later day, so those
// ticks keep the date. A day or wider is a calendar date.
function cumulativeTick(start, seconds) {
  if (!start) return "—";
  const width = Number(seconds);
  if (Number.isFinite(width) && width >= 86400) return formatDate(start);
  if (Number.isFinite(width) && width >= 3600) {
    return `${formatDate(start)} ${formatClock(start, { seconds: false, zone: false })}`;
  }
  if (Number.isFinite(width) && width > 0) return formatClock(start, { seconds: false, zone: false });
  return formatDateTime(start);
}

// Assignments in a finished bucket are those before the next boundary. The
// last bucket is still open: its cumulative values stop at `window.until`
// when that cutoff is sooner than the boundary.
function cumulativeEffectiveEnd(start, seconds, until, last) {
  const opened = new Date(start).getTime();
  if (!Number.isFinite(opened)) return null;
  const width = Number(seconds);
  const boundary = Number.isFinite(width) && width > 0 ? opened + width * 1000 : null;
  if (!last) return boundary;
  const cutoff = until == null || until === "" ? NaN : new Date(until).getTime();
  if (!Number.isFinite(cutoff)) return boundary;
  return boundary == null ? cutoff : Math.min(boundary, cutoff);
}

function cumulativeHover(start, seconds, until, last) {
  const endMs = cumulativeEffectiveEnd(start, seconds, until, last);
  if (!start || endMs == null) return formatDateTime(start);
  const end = new Date(endMs);
  const withSeconds = end.getUTCSeconds() !== 0 || end.getUTCMilliseconds() !== 0;
  const startText = formatDateTime(start);
  const endText = formatDate(start) === formatDate(end)
    ? formatClock(end, { seconds: withSeconds, zone: false })
    : formatDateTime(end, { seconds: withSeconds });
  return `${startText}–${endText}`;
}

function cumulativeCard(readout, primary, color) {
  const series = readout.cumulative;
  const unit = primary?.unit;
  const title = "Cumulative primary metric";
  if (!primary || !series || !(series.arms || []).length) {
    return card(title, [empty("No primary metric", "This experiment names no primary metric to chart.")], { className: "mx-narrow" });
  }
  const arms = series.arms;
  const buckets = Math.max(...arms.map((arm) => (arm.points || []).length), 0);
  const values = arms.flatMap((arm) => (arm.points || []).map((point) => num(point.value))).filter((n) => n != null);
  if (!values.length || buckets === 0) {
    return card(title, [empty("Nothing settled yet", "The series starts when an enrolled task's primary outcome settles.")], { meta: metricLabel(primary.metric), className: "mx-narrow" });
  }
  let lo = Math.min(...values);
  let hi = Math.max(...values);
  if (unit === "rate") { lo = 0; hi = Math.max(hi, 1e-9) > 0.5 ? 1 : Math.min(1, hi * 1.2); }
  else {
    const pad = (hi - lo) * 0.1 || Math.abs(hi) * 0.1 || 1;
    lo = Math.max(0, lo - pad);
    hi += pad;
  }
  const xOf = (index) => (buckets === 1 ? 50 : (index / (buckets - 1)) * 100);
  const yOf = (value) => 100 - ((value - lo) / (hi - lo || 1)) * 100;
  const plot = svg("svg", { viewBox: "0 0 1000 1000", preserveAspectRatio: "none", class: "ex-cum-lines", "aria-hidden": "true" });
  const ends = [];
  arms.forEach((arm) => {
    const colorClass = armColor(color(arm.arm));
    const points = (arm.points || []).map((point, index) => {
      const value = num(point.value);
      return value == null ? null : { x: xOf(index), y: yOf(value) };
    });
    plot.appendChild(svg("path", { d: linePath(points), class: `mx-line ${colorClass}`, "vector-effect": "non-scaling-stroke" }));
    const last = [...points].reverse().find(Boolean);
    if (last) ends.push(el("span", { class: `mx-pt ${colorClass}`, style: { left: `${last.x}%`, top: `${last.y}%` } }));
  });
  const ticks = [hi, (hi + lo) / 2, lo].map((value) =>
    el("span", { class: "ex-cum-ytick", style: { top: `${yOf(value)}%` }, text: fmtValue(value, unit) }));
  const firstPoints = arms[0].points || [];
  const bucketSeconds = series.window?.bucket_seconds;
  const bucketUntil = series.window?.until;
  const xTicks = [0, Math.floor((buckets - 1) / 2), buckets - 1]
    .filter((index, i, all) => all.indexOf(index) === i)
    .map((index) => el("span", {
      class: "ex-cum-xtick",
      style: { left: `${xOf(index)}%` },
      text: cumulativeTick(firstPoints[index]?.start, bucketSeconds),
    }));
  const tip = el("div", { class: "mx-tip ex-cum-tip" });
  tip.hidden = true;
  const cursor = el("div", { class: "mx-cursor" });
  cursor.hidden = true;
  const hover = el("div", { class: "ex-cum-hover" });
  for (let index = 0; index < buckets; index += 1) {
    const column = el("span", { class: "ex-cum-col" });
    const show = () => {
      tip.replaceChildren(
        el("div", { class: "mx-tip-title", text: cumulativeHover(firstPoints[index]?.start, bucketSeconds, bucketUntil, index === buckets - 1) }),
        ...arms.map((arm) => {
          const point = arm.points?.[index] || {};
          return el("div", { class: "mx-tip-row" }, [
            el("span", { class: "mx-tip-name" }, [swatch(armColor(color(arm.arm))), el("span", { text: arm.arm })]),
            el("span", { class: "mx-tip-n", text: `n ${point.n ?? 0}` }),
            el("span", { class: "mx-tip-value", text: fmtValue(point.value, unit) }),
          ]);
        }),
      );
      const left = xOf(index);
      cursor.style.left = `${left}%`;
      tip.hidden = false;
      // This narrow card can sit against the pane's clipped right edge even
      // at its middle bucket. Measure the shown tip, then keep it inside both
      // the plot and the viewport, including when its CSS width changes.
      const bounds = hover.getBoundingClientRect();
      const minLeft = Math.max(bounds.left, 0);
      const maxRight = Math.min(bounds.right, document.documentElement.clientWidth);
      tip.style.maxWidth = `${Math.max(0, maxRight - minLeft)}px`;
      const tipWidth = tip.getBoundingClientRect().width;
      const anchor = bounds.left + bounds.width * left / 100;
      const preferred = anchor + 14 + tipWidth <= maxRight ? anchor + 14 : anchor - 14 - tipWidth;
      tip.style.left = `${Math.max(minLeft, Math.min(preferred, maxRight - tipWidth)) - bounds.left}px`;
      cursor.hidden = false;
    };
    column.addEventListener("mouseenter", show);
    hover.appendChild(column);
  }
  hover.addEventListener("mouseleave", () => { tip.hidden = true; cursor.hidden = true; });
  const legend = el("div", { class: "ex-legend" }, arms.map((arm) =>
    el("span", { class: "ex-legend-item" }, [swatch(armColor(color(arm.arm))), arm.arm])));
  const width = series.window?.bucket_seconds;
  const bucketText = width ? (width % 604800 === 0 ? `${width / 604800}w` : width >= 86400 ? `${width / 86400}d` : width >= 3600 ? `${width / 3600}h` : `${width / 60}m`) : "?";
  const label = `${metricLabel(primary.metric)} cumulative per arm: ${arms.map((arm) => {
    const last = [...(arm.points || [])].reverse().find((point) => num(point.value) != null);
    return `${arm.arm} ${last ? fmtValue(last.value, unit) : "no value"}`;
  }).join(", ")}`;
  return card(title, [
    el("p", { class: "mx-blurb", text: `${metricLabel(primary.metric)} over settled tasks assigned by each ${bucketText} bucket's end.` }),
    legend,
    el("div", { class: "ex-cum", role: "img", "aria-label": label }, [
      el("div", { class: "ex-cum-y" }, ticks),
      el("div", { class: "ex-cum-frame" }, [plot, ...ends, cursor, hover, tip]),
    ]),
    el("div", { class: "ex-cum-x" }, xTicks),
  ], { meta: unitLabel(unit), className: "mx-narrow" });
}

function complexityCard(primary, color) {
  const title = "By complexity";
  const rows = [];
  for (const diff of primary?.differences || []) {
    for (const cell of diff.by_complexity || []) rows.push({ arm: diff.arm, ...cell });
  }
  if (!rows.length) {
    return card(title, [empty("No complexity cells yet", "A cell needs settled tasks in both the arm and control.")], { className: "mx-wide" });
  }
  const unit = primary.unit;
  const table = el("table", { class: "mx-table ex-table" }, [
    el("caption", { text: `${metricLabel(primary.metric)}: arm minus control within each complexity` }),
    el("thead", {}, [el("tr", {}, [
      el("th", { scope: "col", text: "Complexity" }),
      el("th", { scope: "col", text: "Arm" }),
      el("th", { scope: "col", class: "num", text: "n arm" }),
      el("th", { scope: "col", class: "num", text: "n control" }),
      el("th", { scope: "col", class: "num", text: "Difference" }),
      el("th", { scope: "col", class: "num", text: "95% interval" }),
    ])]),
    el("tbody", {}, rows.map((row) => el("tr", {}, [
      el("th", { scope: "row", text: row.complexity }),
      el("td", {}, [el("span", { class: "ex-arm-name" }, [swatch(armColor(color(row.arm))), row.arm])]),
      el("td", { class: "num", text: String(row.n_arm ?? 0) }),
      el("td", { class: "num", text: String(row.n_control ?? 0) }),
      el("td", { class: "num", text: fmtValue(row.estimate, unit, { diff: true }) }),
      el("td", { class: "num", text: fmtInterval(row.interval, unit, { diff: true }) || "—" }),
    ]))),
  ]);
  return card(title, [el("div", { class: "mx-table-wrap", tabindex: 0, role: "region", "aria-label": "Difference by complexity" }, [table])], { className: "mx-wide" });
}

function crewCard(readout, color) {
  const arms = readout.arms || [];
  const rows = arms.map((arm) => {
    const total = (arm.crew_mix || []).reduce((sum, row) => sum + (row.tasks || 0), 0);
    const crews = (arm.crew_mix || []).map((row) =>
      el("span", { class: "ex-crew" }, [
        el("span", { class: "ex-crew-name", text: row.crew }),
        el("span", { class: "ex-crew-n", text: `${row.tasks} · ${total ? Math.round((row.tasks / total) * 100) : 0}%` }),
      ]));
    return el("div", { class: "ex-crew-row" }, [
      el("span", { class: "ex-arm-name" }, [swatch(armColor(color(arm.name))), arm.name]),
      el("span", { class: "ex-crews" }, crews.length ? crews : [el("span", { class: "mx-muted", text: "no tasks" })]),
      el("span", { class: "mx-muted", text: `Recorded effort: ${(arm.effort_breakdown || []).map(item =>
        `${item.crew || "unknown crew"} · ${item.model || "unknown model"} · ${item.reasoning_effort === null ? "provider default" : item.reasoning_effort || "unknown"}: ${item.invocations} invocations`).join("; ") || "unknown"}` }),
    ]);
  });
  return card("Crew mix", [
    el("p", { class: "mx-blurb", text: "Tasks without an explicit crew draw from the arm's configured pool: its overlay pool when set, otherwise the ordinary pool. Different pools can produce different mixes by design; within a pool, weighted draws balance only in expectation." }),
    el("div", { class: "ex-crew-rows" }, rows),
  ], { className: "mx-narrow" });
}

function eventDetail(event) {
  const detail = event.detail && typeof event.detail === "object" ? event.detail : {};
  const weights = detail.weights_after || detail.weights;
  const parts = [];
  if (event.from_status && event.to_status && event.from_status !== event.to_status) {
    parts.push(`${event.from_status} → ${event.to_status}`);
  }
  if (weights && typeof weights === "object") {
    parts.push(`weights ${Object.entries(weights).map(([arm, weight]) => `${arm} ${weight}`).join(" · ")}`);
  }
  if (Object.hasOwn(detail, "arm") && event.kind === "concluded") parts.push(detail.arm ? `arm ${detail.arm}` : "no arm chosen");
  if (event.kind === "concluded" && detail.readout && typeof detail.readout === "object") {
    const label = detail.readout.verdict && detail.readout.verdict.label;
    if (label) parts.push(`readout snapshot ${label}`);
  }
  if (typeof detail.promotion_task === "string" && detail.promotion_task) {
    parts.push(`promotion task ${detail.promotion_task}`);
  }
  if (Array.isArray(detail.retired) && detail.retired.length) parts.push(`retired ${detail.retired.join(", ")}`);
  return parts.join(" · ");
}

function timelineCard(experiment) {
  const events = experiment.events || [];
  if (!events.length) return card("Timeline", [empty("No events", null)]);
  return card("Timeline", [
    el("ol", { class: "ex-timeline" }, events.map((event) => {
      const detail = eventDetail(event);
      return el("li", { class: `ex-event ex-event-${event.kind}` }, [
        el("time", { class: "ex-event-at", datetime: event.at, text: formatDateTime(event.at) }),
        el("span", { class: "ex-event-body" }, [
          el("span", { class: "ex-event-kind", text: EVENT_LABELS[event.kind] || event.kind }),
          el("span", { class: "ex-event-meta", text: [`segment ${event.segment}`, `by ${event.actor}`, detail].filter(Boolean).join(" · ") }),
          event.reason ? el("span", { class: "ex-event-reason", text: event.reason }) : null,
        ]),
      ]);
    })),
  ], { meta: plural(events.length, "event") });
}

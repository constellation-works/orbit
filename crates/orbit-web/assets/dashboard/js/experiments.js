// Orbit dashboard Analytics › Experiments: the workspace's experiments with
// each arm's enrollment toward its minimum sample, and one experiment's
// readout, laid out around the operator's decision. A header holds the answer,
// what it is waiting on, a health summary and the Conclude action, above three
// tabs. Result is what the call rests on: the arm cards and the primary and
// guardrail differences from control. Trust is whether to believe it: health,
// delivery, crew mix, the cumulative series and the per-complexity table. Setup
// is what ran: the definition and event timeline. Definitions and caveats sit
// behind info icons. Drafts are created, edited and started through
// experiment-form.js.
// Read from /api/diagnostics/experiments[/{id}[/readout]]; conclude posts
// to /api/diagnostics/experiments/{id}/conclude. The detail renders from the
// experiment record alone and loads the readout beside it: a slow or failed
// readout holds only the Result and Trust tabs, which show their own loading
// or error state and a Retry, never the header or Setup.
// Pure vanilla JS, split into ES modules with no build step; every node is
// built with DOM calls, never markup strings.
//
// `#analytics/experiments` is the list and `#analytics/experiments/<id>` one
// experiment, with `/trust` or `/setup` naming its other tabs.
// Experiments are per workspace; the aggregate view shows the usual placeholder.

import {
  syncNodes, fetchJson, postJson, requestPanel, panelCanRender, getWorkspaceRevision,
  onWorkspaceChange, renderPanelPlaceholder, isAggregateView, formatDateTime,
  pinnedInfoHint, detailsPanel, scopedHashHref,
} from './common.js';
import { setActiveTab } from './router.js';
import {
  el, num, metricLabel, pluralCount, armColor, armIndex, armSwatch, experimentBadge, experimentCard, experimentEmpty,
  armCards, armEvidenceCard, forestCard, cumulativeCard, complexityCard, crewCard,
} from './experiment-readout.js';
import { draftActions, draftStartRenderState, lockedNote, syncExperimentAuthoring } from './experiment-form.js';

const $ = (id) => document.getElementById(id);

const BODY = "experiments-body";
const COUNT = "experiments-count";
const ENDPOINT = "/api/diagnostics/experiments";
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
  interactive: "Interactive exclusions",
};
const EVENT_LABELS = {
  created: "Created", updated: "Definition edited", cloned: "Cloned", started: "Started",
  paused: "Paused", resumed: "Resumed", reweighted: "Reweighted", arm_retired: "Arm retired",
  concluded: "Concluded", abandoned: "Abandoned",
};

const DETAIL_TABS = [["result", "Result"], ["trust", "Trust"], ["setup", "Setup"]];
// A verdict the operator can act on: the readout has finished collecting and
// nothing withholds it.
const DECIDABLE_VERDICTS = new Set(["winner", "no_difference", "inconclusive"]);
const DEFAULT_DETAIL_TAB = "result";

let selectedId = null;
let detailTab = DEFAULT_DETAIL_TAB;
let tabPickListener = null;
let lastPayload = null;
let requestScope = null;
let draft = null;
// The selected experiment's readout request: `{ scope, readout, error,
// request }`, kept apart from the panel request so the record renders first.
let readoutState = null;

/// The experiment on screen, or null for the list.
export function getExperimentId() {
  return selectedId;
}

/// Select experiment `id` and its tab (an unknown or missing tab is Result).
export function setExperimentId(id, tab = null) {
  const next = id ? String(id) : null;
  if (next !== selectedId) draft = null;
  selectedId = next;
  detailTab = DETAIL_TABS.some(([key]) => key === tab) ? tab : DEFAULT_DETAIL_TAB;
  applyDetailTab();
}

/// The hash route of the view on screen, without the leading `#`.
export function experimentsRoute(id = selectedId) {
  if (!id) return "analytics/experiments";
  const route = `analytics/experiments/${encodeURIComponent(id)}`;
  return id === selectedId && detailTab !== DEFAULT_DETAIL_TAB ? `${route}/${detailTab}` : route;
}

/// `listener` runs when the operator picks a tab, so the router can keep the
/// address in step without routing, which would refetch.
export function onExperimentTabPick(listener) {
  tabPickListener = listener;
}

function clearExperiments() {
  lastPayload = null;
  readoutState = null;
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
  try {
    const experiment = await fetchJson(`${ENDPOINT}/${encodeURIComponent(id)}`);
    return { kind: "detail", id, experiment };
  } catch (error) {
    if (error && (error.status === 400 || error.status === 404) && !error.routeMissing) {
      return { kind: "missing", id, message: error.message };
    }
    throw error;
  }
}

// Read `id`'s readout unless that read is already in flight, as a poll can
// arrive while a slow one runs. A refresh keeps the readout on screen, and a
// failure stays shown until a later read succeeds. Never rejects: the result
// area shows the error.
function loadReadout(id, revision) {
  const scope = `${revision}:${id}`;
  if (readoutState?.scope !== scope) readoutState = { scope, readout: null, error: null, request: null };
  const state = readoutState;
  if (state.request) return state.request;
  state.request = fetchJson(`${ENDPOINT}/${encodeURIComponent(id)}/readout`).then((readout) => {
    state.readout = readout;
    state.error = null;
  }, (error) => {
    state.error = error;
  }).finally(() => {
    state.request = null;
    if (readoutState === state && getWorkspaceRevision() === revision && selectedId === id) renderExperiments();
  });
  return state.request;
}

function retryReadout() {
  const state = readoutState;
  if (!state || state.request || !selectedId) return;
  state.error = null;
  loadReadout(selectedId, getWorkspaceRevision());
  renderExperiments();
}

// The readout as the detail renders it: on screen once read, otherwise
// loading or failed.
function readoutView() {
  const state = readoutState;
  return {
    readout: state?.readout || null,
    error: state?.error ? state.error.message || "The readout could not be loaded." : null,
    pending: Boolean(state?.request),
  };
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
  const readout = id ? loadReadout(id, revision) : null;
  const panel = requestPanel(BODY, `experiments:${id || ""}`, request, (payload) => {
    if (getWorkspaceRevision() !== revision || selectedId !== id) return;
    lastPayload = payload;
    renderExperiments();
  }, COUNT);
  return readout ? panel.then(() => readout) : panel;
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
    const view = readoutView();
    const analysed = view.readout?.enrollment?.analysed ?? 0;
    if (count) {
      count.textContent = view.readout ? `${analysed} task${analysed === 1 ? "" : "s"} analysed`
        : view.error && !view.pending ? "readout unavailable" : "reading results…";
    }
    nodes = [detailView(lastPayload.experiment, view)];
  }
  const root = el("div", { class: "ex-view" }, nodes);
  root.dataset.key = `experiments-${lastPayload.kind}`;
  root.dataset.hash = JSON.stringify([lastPayload, lastPayload.kind === "detail" ? readoutView() : null, draft, draftStartRenderState()]);
  syncNodes(body, [root]);
  applyDetailTab();
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

// ---- list ---------------------------------------------------------------------

function listView(experiments, authoring) {
  if (!experiments.length) {
    return experimentCard("Experiments", [experimentEmpty(
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
        armSwatch(armColor(index)),
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
  if (experiment.forced) extras.push(`${pluralCount(experiment.forced, "forced assignment")} left out`);
  if (experiment.excluded) extras.push(`${experiment.excluded} excluded for a surface conflict`);
  if (experiment.concluded_arm) extras.push(`concluded with ${experiment.concluded_arm}`);
  return el("article", { class: "mx-card ex-item", role: "listitem" }, [
    el("div", { class: "ex-item-head" }, [
      experimentLink(experiment.id, [experiment.name || experiment.id], "ex-item-name"),
      el("span", { class: "ex-badges" }, [
        experimentBadge(experiment.status, `ex-status-${experiment.status}`),
        experimentBadge(experiment.mode, `ex-mode-${experiment.mode}`),
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

// The header and Setup come from the experiment record; Result and Trust wait
// on the readout and say so while it loads or after it fails.
function detailView(experiment, view) {
  const { readout } = view;
  const color = readout ? armIndex(readout) : null;
  const primary = readout
    ? (readout.metrics || []).find((metric) => metric.metric === readout.experiment?.primary_metric) || null
    : null;
  return el("div", { class: "ex-detail" }, [
    headerCard(experiment, view, primary),
    detailTabStrip(),
    detailTabPanel("result", readout ? [
      readout && view.error ? readoutNotice(view) : null,
      armCards(experiment, readout, primary, color),
      forestCard(experiment, readout, color),
    ] : [readoutPlaceholder("Result", view)]),
    detailTabPanel("trust", readout ? [
      readout && view.error ? readoutNotice(view) : null,
      healthCard(readout),
      el("div", { class: "mx-row" }, [
        armEvidenceCard(readout),
        crewCard(readout, color, experiment.id),
      ]),
      el("div", { class: "mx-row" }, [
        complexityCard(primary, color),
        cumulativeCard(readout, primary, color),
      ]),
    ] : [readoutPlaceholder("Trust", view)]),
    detailTabPanel("setup", [
      definitionCard(experiment, readout),
      timelineCard(experiment),
    ]),
  ]);
}

function retryButton(view) {
  const button = el("button", { class: "ex-readout-retry", type: "button", text: view.pending ? "Retrying…" : "Retry" });
  button.disabled = view.pending;
  button.addEventListener("click", retryReadout);
  return button;
}

// A tab that needs the readout, before it has one.
function readoutPlaceholder(title, view) {
  if (view.error) {
    return el("section", { class: "mx-card ex-card ex-readout-state ex-readout-error", role: "alert", "aria-label": title }, [
      el("div", { class: "mx-empty ex-empty" }, [
        el("div", { class: "mx-empty-title", text: "The readout could not be loaded" }),
        el("div", { class: "mx-empty-detail", text: view.error }),
        retryButton(view),
      ]),
    ]);
  }
  return el("section", { class: "mx-card ex-card ex-readout-state ex-readout-loading", role: "status", "aria-label": title, "aria-busy": "true" }, [
    experimentEmpty("Reading results…", "The readout covers every enrolled task, so it can take a while on a large workspace."),
  ]);
}

// A refresh that failed while an earlier readout stays on screen.
function readoutNotice(view) {
  return el("p", { class: "ex-readout-notice", role: "alert" }, [
    el("span", { text: `Showing the last readout; refreshing it failed: ${view.error}` }),
    retryButton(view),
  ]);
}

// The definition as the record states it, so the header and Setup need no
// readout: the first start event opens the horizon, as it does the readout's.
function definitionInfo(experiment) {
  const started = (experiment.events || []).find((event) => event.kind === "started")?.at || null;
  const startedAt = started ? Date.parse(started) : NaN;
  const days = num(experiment.max_days);
  return {
    primary_metric: experiment.primary_metric || null,
    min_sample: num(experiment.min_sample),
    max_days: days,
    started_at: started,
    horizon_at: Number.isFinite(startedAt) && days != null ? new Date(startedAt + days * 86400000).toISOString() : null,
  };
}

// ---- detail tabs ----------------------------------------------------------------
//
// All three stay rendered and a switch only shows another, so it never refetches
// and an open section or focused control survives it.

function applyDetailTab() {
  const body = $(BODY);
  if (!body) return;
  for (const button of body.querySelectorAll(".ex-tab")) {
    const on = button.dataset.exTab === detailTab;
    button.classList.toggle("active", on);
    button.setAttribute("aria-selected", on ? "true" : "false");
    button.tabIndex = on ? 0 : -1;
  }
  for (const panel of body.querySelectorAll(".ex-tab-panel")) panel.hidden = panel.dataset.exTab !== detailTab;
}

function pickDetailTab(tab) {
  detailTab = tab;
  applyDetailTab();
  tabPickListener?.();
}

function detailTabStrip() {
  const strip = el("div", { class: "band-tabs ex-tabs", role: "tablist", "aria-label": "Experiment sections" }, DETAIL_TABS.map(([key, text]) => {
    const button = el("button", {
      id: `ex-tab-${key}`, class: "band-tab ex-tab", type: "button", role: "tab", "aria-controls": `ex-panel-${key}`, text,
    });
    button.dataset.exTab = key;
    button.addEventListener("click", () => pickDetailTab(key));
    return button;
  }));
  strip.addEventListener("keydown", (event) => {
    const keys = DETAIL_TABS.map(([key]) => key);
    const index = keys.indexOf(event.target?.dataset?.exTab);
    if (index < 0) return;
    let next = null;
    if (event.key === "ArrowRight") next = (index + 1) % keys.length;
    else if (event.key === "ArrowLeft") next = (index - 1 + keys.length) % keys.length;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = keys.length - 1;
    if (next == null) return;
    event.preventDefault();
    pickDetailTab(keys[next]);
    $(`ex-tab-${keys[next]}`)?.focus();
  });
  return strip;
}

function detailTabPanel(key, children) {
  const panel = el("div", { id: `ex-panel-${key}`, class: "ex-tab-panel", role: "tabpanel", "aria-labelledby": `ex-tab-${key}` }, children);
  panel.dataset.exTab = key;
  return panel;
}

// ---- detail header ---------------------------------------------------------------

// Beside an operator's conclusion, what the readout itself said: the two can
// differ, as when a sample-ratio mismatch withholds its verdict.
function readoutVerdictBadge(experiment, verdict) {
  if (experiment.status !== "concluded") return null;
  const state = verdict?.state || "collecting";
  return experimentBadge(`readout: ${verdict?.label || state}`, `ex-verdict ex-verdict-${state}`);
}

// The headline answer: the operator's conclusion once there is one, otherwise
// the readout's own verdict.
function outcomeHeadline(experiment, verdict) {
  if (experiment.status === "concluded") {
    return experiment.concluded_arm
      ? { text: `Winner: ${experiment.concluded_arm}`, state: "winner" }
      : { text: "No winner", state: "inconclusive" };
  }
  if (experiment.status === "abandoned") return { text: "Abandoned", state: "abandoned" };
  if (experiment.status === "draft") return { text: "Not started", state: "collecting" };
  const state = verdict?.state || "collecting";
  return { text: verdict?.label || state, state };
}

// The slowest arm's settled primary outcomes against the minimum sample.
function smallestArmText(primary, minSample) {
  const settled = (primary?.arms || []).map((row) => num(row.n)).filter((n) => n != null);
  if (!settled.length) return minSample != null ? `min ${minSample}` : "—";
  const smallest = Math.min(...settled);
  if (minSample == null) return `${smallest} settled`;
  return smallest < minSample ? `${smallest} of ${minSample} settled` : `${smallest} settled (min ${minSample})`;
}

function headerCard(experiment, view, primary) {
  const { readout } = view;
  const info = definitionInfo(experiment);
  const verdict = readout?.verdict || null;
  const headline = readout || !needsReadoutVerdict(experiment)
    ? outcomeHeadline(experiment, verdict)
    : { text: view.error ? "Readout unavailable" : "Reading results…", state: "collecting" };
  const concluded = concludedEvent(experiment);
  const notes = [
    concluded?.reason ? `Reason: ${concluded.reason}` : null,
    ...(verdict?.reasons || []).map((reason) => `${reason}.`),
    readout ? `As of ${formatDateTime(readout.as_of)}.` : null,
  ].filter(Boolean);
  const promotion = concluded?.detail?.promotion_task;
  return el("section", { class: "mx-card ex-card ex-head", "aria-label": "Experiment" }, [
    experimentLink(null, ["All experiments"], "ex-back"),
    el("div", { class: "ex-item-head" }, [
      el("h2", { class: "ex-title" }, [
        experiment.name || experiment.id,
        experiment.hypothesis ? pinnedInfoHint(experiment.hypothesis) : null,
      ]),
      el("span", { class: "ex-badges" }, [
        experimentBadge(experiment.status, `ex-status-${experiment.status}`),
        experimentBadge(experiment.mode, `ex-mode-${experiment.mode}`),
      ]),
    ]),
    el("div", { class: "ex-decision" }, [
      el("div", { class: "ex-outcome" }, [
        el("p", { class: `ex-outcome-text ex-outcome-${headline.state}` }, [
          el("span", { class: "ex-outcome-label", text: headline.text }),
          notes.length ? pinnedInfoHint(notes.join("\n")) : null,
        ]),
        readoutVerdictBadge(experiment, verdict),
      ]),
      factList([
        ["Smallest arm", readout ? smallestArmText(primary, info.min_sample) : pendingFact(view)],
        concluded ? ["Concluded", formatDateTime(concluded.at)] : ["Horizon", horizonText(info)],
        typeof promotion === "string" && promotion ? ["Promotion task", taskLink(promotion)] : null,
        ["Health", readout ? healthLink(readout) : pendingFact(view)],
      ].filter(Boolean)),
      concludeMenu(experiment, verdict),
    ]),
    draftActions(experiment, experiment.authoring),
  ]);
}

// Only a running or paused experiment's headline is the readout's verdict.
function needsReadoutVerdict(experiment) {
  return experiment.status !== "concluded" && experiment.status !== "abandoned" && experiment.status !== "draft";
}

function pendingFact(view) {
  return view.error && !view.pending ? "unavailable" : "reading…";
}

function horizonText(info) {
  if (info.horizon_at) return formatDateTime(info.horizon_at);
  return info.max_days != null ? `${info.max_days} days after start` : "—";
}

function factList(facts) {
  return el("dl", { class: "ex-facts" }, facts.map(([label, value]) =>
    el("div", { class: "ex-fact" }, [el("dt", { text: label }), el("dd", {}, [value])])));
}

function definitionCard(experiment, readout) {
  const info = definitionInfo(experiment);
  const facts = [
    ["Status", experiment.status],
    ["Mode", experiment.mode === "shadow" ? "shadow (assigns, applies nothing)" : experiment.mode],
    ["Segment", String(experiment.segment ?? 0)],
    ["Primary metric", info.primary_metric ? metricLabel(info.primary_metric) : "—"],
    ["Minimum sample", info.min_sample != null ? `${info.min_sample} per arm` : "—"],
    ["Started", info.started_at ? formatDateTime(info.started_at) : "not started"],
    ["Horizon", horizonText(info)],
    readout ? ["As of", formatDateTime(readout.as_of)] : null,
  ].filter(Boolean);
  if (experiment.concluded_arm) facts.splice(2, 0, ["Concluded with", experiment.concluded_arm]);
  else if (experiment.status === "concluded") facts.splice(2, 0, ["Concluded with", "no winner"]);
  return experimentCard("Definition", [
    experiment.hypothesis ? el("p", { class: "ex-hypothesis", text: experiment.hypothesis }) : null,
    factList(facts),
    lockedNote(experiment, experiment.authoring),
  ]);
}

function concludedEvent(experiment) {
  return (experiment.events || []).find((item) => item.kind === "concluded") || null;
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

// Conclude sits beside the answer it acts on, and stands out once the readout
// has one. The form opens over the page, so it never sits between the verdict
// and the arms, and its open state survives a poll that rebuilds the page.
function concludeMenu(experiment, verdict) {
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
    const called = verdict?.state === "winner" && verdict.arm === arm.name;
    const option = el("option", { text: `${arm.name}${arm.control ? " (control)" : ""}${called ? " (readout's winner)" : ""}` });
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
  const menu = detailsPanel(`experiment-conclude:${experiment.id}`, { class: "ex-conclude" });
  menu.append(
    el("summary", { class: `ex-conclude-open${DECIDABLE_VERDICTS.has(verdict?.state) ? " ex-conclude-ready" : ""}`, text: "Conclude" }),
    el("div", { class: "ex-conclude-body", role: "group", "aria-label": "Conclude experiment" }, [
      el("h2", { class: "mx-card-title" }, [
        "Conclude",
        pinnedInfoHint("Choosing an arm files one proposed task to make that arm's overlay the default. No winner files nothing. This does not change configuration or repository files."),
      ]),
      el("div", { class: "ex-conclude-fields" }, [
        el("label", { class: "ex-conclude-field" }, [el("span", { text: "Winning arm" }), select]),
        el("label", { class: "ex-conclude-field" }, [el("span", { text: "Reason" }), reason]),
      ]),
      el("div", { class: "ex-conclude-actions" }, [button]),
      status,
    ]),
  );
  return menu;
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
  } catch (error) {
    if (draft !== state) return;
    state.status = error && error.message ? error.message : "Conclude failed.";
    state.pending = false;
    renderExperiments();
    return;
  }
  if (draft !== state) return;
  // The conclusion landed. A failed re-read is not a refused conclude: keep
  // the form disabled (a retry would be refused) and say the page is stale
  // until the next poll reads the concluded experiment.
  state.status = "Concluded.";
  try {
    await fetchAndRenderExperiments();
  } catch (_) {
    if (draft !== state) return;
    state.status = "Concluded. Reloading the experiment failed; it updates on the next refresh.";
    renderExperiments();
  }
}

// Everything that qualifies the readout: its own warnings, then the tasks it
// left out.
function healthWarnings(readout) {
  const warnings = [...(readout.warnings || [])];
  const forced = readout.enrollment?.forced_excluded || 0;
  if (forced) {
    warnings.push({
      kind: "forced",
      message: `${pluralCount(forced, "forced assignment")} ${forced === 1 ? "is" : "are"} left out of every estimate`,
    });
  }
  const explicitCrew = readout.enrollment?.explicit_crew_excluded || 0;
  if (explicitCrew) {
    warnings.push({
      kind: "explicit_crew",
      message: `${pluralCount(explicitCrew, "task")} with an explicit crew ${explicitCrew === 1 ? "was" : "were"} excluded, since no crew arm would have changed ${explicitCrew === 1 ? "its" : "their"} crew`,
    });
  }
  const interactive = readout.enrollment?.interactive_excluded || 0;
  if (interactive) {
    warnings.push({
      kind: "interactive",
      message: `${pluralCount(interactive, "task")} worked in an interactive session ${interactive === 1 ? "was" : "were"} excluded, since no job applied ${interactive === 1 ? "its" : "their"} arm`,
    });
  }
  return warnings;
}

function warningLabel(warning) {
  return WARNING_LABELS[warning.kind] || warning.kind;
}

// The header's one-line health: enough to tell whether to look, and the way to
// the Trust tab that says why.
function healthLink(readout) {
  const warnings = healthWarnings(readout);
  const text = warnings.length === 1 ? warningLabel(warnings[0]) : warnings.length ? `${warnings.length} warnings` : "Healthy";
  const link = el("button", {
    class: `ex-health-link${warnings.length ? " ex-health-warn" : ""}`,
    type: "button",
    title: warnings.map(warningLabel).join(", ") || null,
  }, [
    warnings.length ? el("span", { class: "ex-warning-icon", "aria-hidden": "true", text: "!" }) : null,
    text,
  ]);
  link.addEventListener("click", () => pickDetailTab("trust"));
  return link;
}

// A named chip per warning, its message behind the icon.
function healthCard(readout) {
  const warnings = healthWarnings(readout);
  const lagging = readout.lagging || {};
  const chips = warnings.length
    ? warnings.map((warning) => el("li", { class: `ex-warning ex-warning-${warning.kind}` }, [
      el("span", { class: "ex-warning-icon", "aria-hidden": "true", text: "!" }),
      el("span", { class: "ex-warning-kind", text: warningLabel(warning) }),
      pinnedInfoHint(warning.message),
    ]))
    : [el("li", { class: "ex-healthy" }, [
      "Healthy",
      pinnedInfoHint("No warnings: arm counts match the weights, no drift, no open outcomes."),
    ])];
  return experimentCard("Health", [el("ul", { class: "ex-warnings" }, chips)], {
    meta: `${lagging.open_tasks ?? 0} in flight · ${lagging.open_regression_windows ?? 0} in the ${lagging.regression_window_days ?? 14}-day regression window`,
  });
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
  if (!events.length) return experimentCard("Timeline", [experimentEmpty("No events", null)]);
  return experimentCard("Timeline", [
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
  ], { meta: pluralCount(events.length, "event") });
}

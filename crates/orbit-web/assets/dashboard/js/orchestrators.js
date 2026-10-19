// Analytics › Orchestrators: ranks the orchestrators named by tasks'
// orchestrator attribution by the work they file and supervise. Reads
// GET /api/orchestrators/scorecard for the rows and the definitions, and
// GET /api/orchestrators/detail when a count is opened. Separate from
// /api/metrics/orchestrators and the Cost view.
//
// The component columns come first, grouped as the payload's definitions
// group them; the composite follows with its weights. Every count shows its
// rate per 100 owned tasks or per 8 active hours (the toggle), and opens the
// tasks behind it. Rows below the minimum support are greyed and sort last.
// Pure vanilla JS, no build step; every node is built with `el()`.

import { el, syncNodes, fetchJson, getWindow, payloadHonorsWindow, wireWindowSelector, requestPanel, getWorkspaceRevision, onWorkspaceChange, renderPanelPlaceholder, isAggregateView, bindTaskLink, formatDateTime } from './common.js';

const $ = (id) => document.getElementById(id);

const BODY_ID = "orchestrators-body";
const COUNT_ID = "orchestrators-count";
const META_ID = "orchestrators-meta";
const RATE_ID = "orchestrators-rate";
const MIN_SUPPORT = 10;
const RATES = {
  per_100_tasks: { label: "per 100 owned tasks", short: "/100" },
  per_8_hours: { label: "per 8 active hours", short: "/8h" },
};
const COMPOSITE = "composite";

let rate = "per_100_tasks";
// `null` keeps the server's order: rows at minimum support first, by volume.
let sort = null;
let lastData = null;
let requestScope = null;
// The open drill-down: which count, and the detail payload once it arrived.
let open = null;
let detailRequest = 0;
const detailCache = new Map();
// The tiles whose definitions the reader opened.
const openDefinitions = new Set();

function scorecardPath(selectedWindow) {
  return `/api/orchestrators/scorecard?window=${encodeURIComponent(selectedWindow)}&min_support=${MIN_SUPPORT}`;
}

function detailPath(selectedWindow, orchestrator) {
  return `/api/orchestrators/detail?window=${encodeURIComponent(selectedWindow)}&min_support=${MIN_SUPPORT}&orchestrator=${encodeURIComponent(orchestrator)}`;
}

function resetView() {
  lastData = null;
  open = null;
  detailRequest += 1;
  detailCache.clear();
  const meta = $(META_ID);
  if (meta) meta.textContent = "";
}

export function placeholdOrchestratorsAggregate() {
  requestScope = null;
  resetView();
  renderPanelPlaceholder(BODY_ID);
  const count = $(COUNT_ID);
  if (count) count.textContent = "—";
}

export function fetchAndRenderOrchestrators() {
  if (isAggregateView()) {
    placeholdOrchestratorsAggregate();
    return Promise.resolve();
  }
  const selectedWindow = getWindow();
  const revision = getWorkspaceRevision();
  const path = scorecardPath(selectedWindow);
  const scope = `${revision}\0${path}`;
  if (requestScope !== scope) {
    requestScope = scope;
    resetView();
  }
  return requestPanel(BODY_ID, path, () => fetchJson(path), (payload) => {
    if (getWorkspaceRevision() !== revision || getWindow() !== selectedWindow) return;
    if (!payloadHonorsWindow(payload, selectedWindow)) {
      console.error(`orchestrator scorecard window ${payload && payload.window} rejected under ${selectedWindow} selection`);
      return;
    }
    const priorComputedAt = lastData && lastData.computed_at;
    const freshScan = Boolean(priorComputedAt && payload && payload.computed_at && payload.computed_at !== priorComputedAt);
    lastData = payload;
    if (freshScan) {
      detailCache.clear();
      if (open) {
        loadDetail(open.orchestrator, open.key);
        return;
      }
    }
    render();
  }, COUNT_ID);
}

onWorkspaceChange(() => {
  requestScope = null;
  resetView();
});

/// Wire the window selector (the shared dashboard window) and the rate toggle.
export function wireOrchestratorControls() {
  wireWindowSelector("orchestrators-window-selector", { allowAll: true });
  const toggle = $(RATE_ID);
  if (!toggle || toggle.dataset.wired === "true") return;
  toggle.dataset.wired = "true";
  for (const button of toggle.querySelectorAll("button")) {
    button.addEventListener("click", () => setRate(button.dataset.value));
  }
  syncRateToggle();
}

function setRate(next) {
  if (!RATES[next] || next === rate) return;
  rate = next;
  syncRateToggle();
  if (lastData) render();
}

function syncRateToggle() {
  const toggle = $(RATE_ID);
  if (!toggle) return;
  for (const button of toggle.querySelectorAll("button")) {
    const on = button.dataset.value === rate;
    button.classList.toggle("on", on);
    button.setAttribute("aria-pressed", on ? "true" : "false");
  }
}

function definitions() {
  return (lastData && lastData.definitions) || {};
}

function components() {
  return Array.isArray(definitions().components) ? definitions().components : [];
}

function componentLabel(key) {
  if (key === COMPOSITE) return "Composite";
  const found = components().find((component) => component.key === key);
  return found ? found.label : key;
}

function number(value) {
  if (typeof value !== "number" || !Number.isFinite(value)) return "—";
  return Number.isInteger(value) ? String(value) : value.toFixed(1);
}

function plural(n, one, many = `${one}s`) {
  return `${n} ${n === 1 ? one : many}`;
}

function rateOf(row, key) {
  if (key === COMPOSITE) return row.composite ? row.composite[rate] : null;
  return row[rate] ? row[rate][key] : null;
}

function countOf(row, key) {
  if (key === COMPOSITE) return row.composite ? row.composite.value : 0;
  return row.counts ? row.counts[key] || 0 : 0;
}

function sortedRows() {
  const rows = Array.isArray(lastData && lastData.rows) ? [...lastData.rows] : [];
  if (!sort) return rows;
  const value = (row) => {
    const rated = rateOf(row, sort.key);
    return typeof rated === "number" ? rated : -Infinity;
  };
  const direction = sort.dir === "asc" ? 1 : -1;
  // Rates below the minimum support are not comparable, so they stay last.
  return rows.sort((a, b) => (Number(!!a.below_support) - Number(!!b.below_support))
    || direction * (value(a) - value(b))
    || String(a.orchestrator).localeCompare(String(b.orchestrator)));
}

function setSort(key) {
  sort = sort && sort.key === key
    ? (sort.dir === "desc" ? { key, dir: "asc" } : null)
    : { key, dir: "desc" };
  render();
}

function render() {
  const body = $(BODY_ID);
  if (!body || !lastData) return;
  const rows = Array.isArray(lastData.rows) ? lastData.rows : [];
  const count = $(COUNT_ID);
  if (count) count.textContent = String(rows.length);
  const meta = $(META_ID);
  if (meta) {
    meta.textContent = `${plural(lastData.attributed_tasks || 0, "attributed task")} created in ${lastData.window}`
      + ` · ${plural(lastData.unattributed_tasks || 0, "task")} with no orchestrator`
      + ` · rows under ${lastData.min_support} owned tasks are greyed`;
  }
  const nodes = [tilesNode()];
  if (!rows.length) {
    const empty = el("p", { class: "orch-empty", text: `No task created in the ${lastData.window} window names an orchestrator. Widen the window.` });
    empty.dataset.key = "empty";
    nodes.push(empty);
  } else {
    nodes.push(tableNode(sortedRows()));
  }
  if (open) nodes.push(detailNode());
  syncNodes(body, nodes);
}

// The definitions, one tile per component group, then the composite's tile
// with its weights. Each tile names its counts; their definitions sit behind
// the tile's disclosure, which stays open across polls and toggles.
function definitionsDisclosure(key, children) {
  const details = el("details", { class: "orch-tile-defs" }, [el("summary", { text: "Definitions" }), ...children]);
  details.dataset.definitions = key;
  details.open = openDefinitions.has(key);
  details.addEventListener("toggle", () => {
    if (details.open) openDefinitions.add(key);
    else openDefinitions.delete(key);
  });
  return details;
}

function tilesNode() {
  const defs = definitions();
  const groups = Array.isArray(defs.groups) ? defs.groups : [];
  const tiles = groups.map((group) => {
    const members = components().filter((entry) => entry.group === group.key);
    const list = el("dl", { class: "orch-defs" });
    for (const component of members) {
      list.append(el("dt", { text: component.label }));
      list.append(el("dd", { text: component.definition }));
    }
    const disclosed = [list];
    if (group.key === "rework" && Array.isArray(defs.causes)) {
      const causes = el("dl", { class: "orch-defs orch-causes" });
      for (const cause of defs.causes) {
        causes.append(el("dt", { text: `Cause: ${cause.key}` }));
        causes.append(el("dd", { text: cause.definition }));
      }
      disclosed.push(causes);
    }
    const tile = el("section", { class: "orch-tile", "aria-label": `${group.label} definitions` }, [
      el("h3", { text: group.label }),
      el("p", { class: "orch-tile-members", text: members.map((component) => component.label).join(" · ") }),
      definitionsDisclosure(group.key, disclosed),
    ]);
    tile.dataset.group = group.key;
    return tile;
  });
  const composite = defs.composite || {};
  const weights = el("ul", { class: "orch-weights" });
  for (const weight of Array.isArray(composite.weights) ? composite.weights : []) {
    const sign = weight.weight < 0 ? "−" : "+";
    weights.append(el("li", { text: `${sign}${Math.abs(weight.weight)} × ${componentLabel(weight.key)}` }));
  }
  const normalization = defs.normalization || {};
  const compositeTile = el("section", { class: "orch-tile orch-tile-composite", "aria-label": "Composite definition" }, [
    el("h3", { text: "Composite" }),
    weights,
    definitionsDisclosure(COMPOSITE, [
      el("p", { text: composite.definition || "" }),
      el("p", { class: "orch-norm", text: normalization[rate] || "" }),
    ]),
  ]);
  compositeTile.dataset.group = COMPOSITE;
  const node = el("div", { class: "orch-tiles" }, [...tiles, compositeTile]);
  node.dataset.key = "tiles";
  node.dataset.hash = JSON.stringify([defs, rate]);
  return node;
}

function sortHeader(key, label, extraClass = "") {
  const current = sort && sort.key === key ? sort.dir : null;
  const button = el("button", { class: "orch-sort", type: "button", text: label, title: `Sort by ${label} ${RATES[rate].label}` });
  button.dataset.sort = key;
  button.addEventListener("click", () => setSort(key));
  const th = el("th", { class: `orch-num ${extraClass}`.trim() }, [button]);
  th.setAttribute("aria-sort", current === "asc" ? "ascending" : current === "desc" ? "descending" : "none");
  return th;
}

function tableNode(rows) {
  const defs = definitions();
  const groups = Array.isArray(defs.groups) ? defs.groups : [];
  const groupRow = el("tr", { class: "orch-group-row" }, [el("th", { text: "" }), el("th", { text: "" })]);
  const headRow = el("tr", {}, [el("th", { text: "Orchestrator" }), el("th", { text: "Support" })]);
  for (const group of groups) {
    const members = components().filter((component) => component.group === group.key);
    if (!members.length) continue;
    const th = el("th", { class: "orch-group", text: group.label });
    th.setAttribute("colspan", String(members.length));
    groupRow.append(th);
    for (const component of members) headRow.append(sortHeader(component.key, component.label));
  }
  const compositeGroup = el("th", { class: "orch-group orch-composite", text: "Composite" });
  groupRow.append(compositeGroup);
  headRow.append(sortHeader(COMPOSITE, `Composite ${RATES[rate].short}`, "orch-composite"));

  const bodyRows = rows.map((row) => {
    const support = row.support || {};
    const name = String(row.orchestrator || "");
    const nameCell = el("th", { class: "orch-name", text: name });
    nameCell.setAttribute("scope", "row");
    if (row.below_support) nameCell.append(el("span", { class: "orch-below", text: "below support" }));
    const supportCell = el("td", { class: "orch-support" }, [
      el("span", { class: "orch-rate", text: plural(support.owned_tasks || 0, "task") }),
      el("span", { class: "orch-count", text: `${number(support.active_hours)} h active` }),
    ]);
    supportCell.title = `${support.tasks_with_runs || 0} of ${support.owned_tasks || 0} owned tasks ran; ${number(support.active_hours)} active hours`;
    const cells = components().map((component) => countCell(row, component));
    const composite = el("td", { class: "orch-num orch-composite", text: number(rateOf(row, COMPOSITE)) });
    composite.title = `Composite ${number(countOf(row, COMPOSITE))}, ${number(rateOf(row, COMPOSITE))} ${RATES[rate].label}`;
    const tr = el("tr", { class: row.below_support ? "orch-row orch-row-below" : "orch-row" }, [nameCell, supportCell, ...cells, composite]);
    tr.dataset.orchestrator = name;
    if (row.below_support) tr.setAttribute("aria-description", `fewer than ${lastData.min_support} owned tasks; rates are not comparable`);
    return tr;
  });
  const table = el("table", { class: "orch-table" }, [
    el("thead", {}, [groupRow, headRow]),
    el("tbody", {}, bodyRows),
  ]);
  const node = el("div", { class: "orch-table-wrap" }, [table]);
  node.dataset.key = "table";
  node.dataset.hash = JSON.stringify([lastData.rows, rate, sort, open && [open.orchestrator, open.key]]);
  return node;
}

function countCell(row, component) {
  const count = countOf(row, component.key);
  const rated = rateOf(row, component.key);
  const td = el("td", { class: "orch-num" });
  td.dataset.component = component.key;
  const value = el("span", { class: "orch-rate", text: number(rated) });
  const raw = el("span", { class: "orch-count", text: String(count) });
  let causeLine = null;
  if (component.key === "rework" && row.rework_by_cause) {
    // Spec and crew rework are the split the scorecard exists to tell apart;
    // the full split is in the title and every drilled occurrence.
    const byCause = row.rework_by_cause;
    causeLine = el("span", { class: "orch-count orch-cause-split", text: `spec ${byCause.spec || 0} · crew ${byCause.crew || 0}` });
    causeLine.title = Object.entries(byCause).map(([cause, n]) => `${cause} ${n}`).join(", ");
  }
  if (component.key === "landed_initiative" && typeof row.landed_share === "number") {
    raw.textContent = `${count} · ${Math.round(row.landed_share * 100)}% landed`;
  }
  if (!count) {
    td.append(value);
    td.append(raw);
    td.title = `${component.label}: none`;
    return td;
  }
  if (causeLine) td.title = `Rework by cause: ${causeLine.title}`;
  const isOpen = open && open.orchestrator === row.orchestrator && open.key === component.key;
  const button = el("button", {
    class: isOpen ? "orch-cell on" : "orch-cell",
    type: "button",
    title: `${component.label}: ${count} (${number(rated)} ${RATES[rate].label}). Show the tasks.`,
    "aria-pressed": isOpen ? "true" : "false",
  }, [value, raw, causeLine]);
  button.dataset.orchestrator = row.orchestrator;
  button.dataset.component = component.key;
  button.addEventListener("click", () => openCount(row.orchestrator, component.key));
  td.append(button);
  return td;
}

function detailCacheKey(orchestrator) {
  const computedAt = (lastData && lastData.computed_at) || "";
  return `${requestScope}\0${computedAt}\0${orchestrator}`;
}

function loadDetail(orchestrator, key) {
  const scope = requestScope;
  const cacheKey = detailCacheKey(orchestrator);
  open = { orchestrator, key, detail: detailCache.get(cacheKey) || null, error: null };
  render();
  if (open.detail) return;
  const selectedWindow = getWindow();
  const request = ++detailRequest;
  fetchJson(detailPath(selectedWindow, orchestrator)).then((detail) => {
    detailCache.set(cacheKey, detail);
    if (request !== detailRequest || scope !== requestScope || !open || open.orchestrator !== orchestrator) return;
    open = { ...open, detail };
    render();
  }, (error) => {
    if (request !== detailRequest || scope !== requestScope || !open || open.orchestrator !== orchestrator) return;
    open = { ...open, error: error.message || String(error) };
    render();
  });
}

function openCount(orchestrator, key) {
  if (open && open.orchestrator === orchestrator && open.key === key) {
    open = null;
    render();
    return;
  }
  loadDetail(orchestrator, key);
}

function detailNode() {
  const { orchestrator, key, detail, error } = open;
  const label = componentLabel(key);
  const definition = (components().find((component) => component.key === key) || {}).definition || "";
  const close = el("button", { class: "orch-close", type: "button", text: "Close", "aria-label": `Close ${label} for ${orchestrator}` });
  close.addEventListener("click", () => { open = null; render(); });
  const children = [
    el("header", { class: "orch-detail-head" }, [el("h3", { text: `${orchestrator} › ${label}` }), close]),
    el("p", { class: "orch-detail-def", text: definition }),
  ];
  if (error) {
    children.push(el("p", { class: "orch-detail-error", role: "alert", text: `Unable to load the tasks: ${error}` }));
  } else if (!detail) {
    children.push(el("p", { class: "orch-detail-loading", role: "status", text: "Loading the tasks…" }));
  } else {
    const component = (detail.components || []).find((entry) => entry.key === key) || { tasks: [] };
    const list = el("ul", { class: "orch-tasks" });
    for (const task of component.tasks || []) list.append(taskItem(task));
    if (!list.children.length) list.append(el("li", { class: "orch-empty", text: "No task in this window." }));
    children.push(el("p", { class: "orch-detail-count", text: `${plural(component.count || 0, "count")} from ${plural((component.tasks || []).length, "task")}` }));
    children.push(list);
  }
  const node = el("section", { class: "orch-detail", "aria-label": `${label} tasks for ${orchestrator}` }, children);
  node.id = "orchestrators-detail";
  node.dataset.key = "detail";
  node.dataset.hash = JSON.stringify([orchestrator, key, (lastData && lastData.computed_at) || "", detail, error]);
  return node;
}

function taskItem(task) {
  const link = bindTaskLink(el("a", { class: "orch-task-id", text: task.task_id }), task.task_id);
  const head = el("div", { class: "orch-task-head" }, [
    link,
    el("span", { class: "orch-task-title", text: task.title || "" }),
    el("span", { class: "orch-task-meta", text: [task.status, task.origin || "no origin", task.complexity].filter(Boolean).join(" · ") }),
  ]);
  if (Math.abs(task.count) !== 1) head.append(el("span", { class: "orch-task-count", text: `×${task.count}` }));
  const item = el("li", { class: "orch-task" }, [head]);
  item.dataset.taskId = task.task_id;
  const occurrences = Array.isArray(task.occurrences) ? task.occurrences : [];
  if (occurrences.length) {
    const list = el("ul", { class: "orch-occurrences" });
    for (const occurrence of occurrences) {
      const line = el("li", {}, [
        el("time", { text: formatDateTime(occurrence.at) }),
        el("span", { text: occurrence.detail || "" }),
      ]);
      if (occurrence.cause) line.append(el("span", { class: `orch-cause orch-cause-${occurrence.cause}`, text: occurrence.cause }));
      list.append(line);
    }
    item.append(list);
  }
  return item;
}

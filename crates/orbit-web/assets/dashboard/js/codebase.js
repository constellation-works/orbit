// Analytics › Codebase: which paths are costly for agents to change, ranked
// from records Orbit already keeps (/api/codebase/analytics). "Touched" is the
// declared footprint, not the landed diff. The server caches one scan per
// workspace; the window, minimum support and rollup only slice it, so the 30 s
// poll never rescans bundles. How well task pilots predicted landed diffs is
// graded under Agents › Pilots.
//
// Four views, each a hash route that keeps the selection's query:
//   #analytics/codebase             Hotspots: one table, a column per path signal
//   #analytics/codebase/file?path=  one path: its signals, regressed tasks and
//                                   the files that land with it (/api/codebase/related)
//   #analytics/codebase/coupling    reserved-together pairs and lock waits
//   #analytics/codebase/marks       the regression-risk marks comparison
// The other view choices (Show all, the path search, release grouping, the
// lock sort) live here, not in the hash. Every node is keyed and hashed for
// `syncNodes`, so a poll that answers the same payload leaves the page exactly
// as it was.

import { captureWorkspaceVisit, onWorkspaceChange, el, fetchJson, isAggregateView, renderPanelPlaceholder, requestPanel, scopedHashHref, syncNodes, taskHref, formatDateTime } from './common.js';

const $ = (id) => document.getElementById(id);

const BODY_ID = "codebase-body";
const WINDOWS = ["7d", "30d", "90d", "all"];
const ROLLUPS = ["file", "dir", "crate"];
const VIEWS = ["hotspots", "file", "coupling", "marks"];
// The dimensions a regression is filtered by, each value in the server's
// breakdown order. `absent` (a link stored without a classification) and
// `unclassified` (the filer could not tell) stay apart.
const REGRESSION_DIMENSIONS = [
  { key: "nature", label: "Nature", values: ["broke_existing", "defect_in_new", "incomplete", "merge_reintroduced", "contract_drift", "unclassified", "absent"] },
  { key: "escape", label: "Escape", values: ["no_test", "test_not_selected", "platform_only", "review_missed", "spec_ambiguous", "nondeterministic", "unclassified", "absent"] },
  { key: "layer", label: "Layer", values: ["ci_tests", "docs_skills", "dashboard", "sandbox_policy", "store_migration", "vcs_delivery", "config", "cli_mcp", "runtime_lifecycle", "other", "unknown"] },
  { key: "detected_by", label: "Detected by", values: ["delivery_review", "security_review", "code_review", "qa_sweep", "post_merge_ci", "production_run", "operator", "other"] },
];
const VALUE_LABELS = {
  broke_existing: "broke existing", defect_in_new: "defect in new code", merge_reintroduced: "merge reintroduced",
  contract_drift: "contract drift", no_test: "no test", test_not_selected: "test not selected", platform_only: "platform only",
  review_missed: "review missed", spec_ambiguous: "spec ambiguous", ci_tests: "CI/tests", docs_skills: "docs/skills",
  sandbox_policy: "sandbox/policy", store_migration: "store/migration", vcs_delivery: "VCS/delivery", cli_mcp: "CLI/MCP surface",
  runtime_lifecycle: "runtime/lifecycle", delivery_review: "delivery review", security_review: "security review",
  code_review: "code review", qa_sweep: "QA sweep", post_merge_ci: "post-merge CI", production_run: "production run",
};
const valueLabel = (value) => VALUE_LABELS[value] || String(value ?? "").replace(/_/g, " ");
const DEFAULT_FILTER = Object.fromEntries(REGRESSION_DIMENSIONS.map((dimension) => [dimension.key, null]));
const DEFAULT_QUERY = { window: "90d", min_support: 15, rollup: "file", ...DEFAULT_FILTER };
// The minimum-support choices; a hashed value off the ladder is added to it.
const SUPPORT_STEPS = [1, 2, 3, 5, 10, 15, 20, 30, 50, 100];
// "Show paths with 5+ tasks" and "Lower to 5".
const LOWERED_SUPPORT = 5;
// A task that regressed this many ranked paths is marked: one fix there lifts
// several rows.
const WIDE_TASK_PATHS = 5;
// Regressed tasks listed on a file before "All N".
const TASKS_SHOWN = 3;
// Paths each signal contributes to "One signal" before Show all.
const SINGLES_SHOWN = 3;
// Unranked known paths a search offers to open.
const OTHER_PATHS_SHOWN = 8;
// Partners the file view's "Changes with" lists.
const RELATED_TOP = 10;
// The files a release bump changes together. Their pairs say only that a
// release happened, so Coupling & locks folds them into one row by default.
const RELEASE_FILES = [
  "Cargo.toml", "Cargo.lock", "CHANGELOG.md", "npm/package.json", "server.json",
  "plugin/plugin.json", "plugin/.claude-plugin/plugin.json", "plugin/.codex-plugin/plugin.json",
];

// The path signals, in column order. Each has one colour, set on the element
// that carries data-signal and read by its dots and shaded cells.
const SIGNALS = ["regressions", "footprint_misses", "blocked", "rejected"];
const SIGNAL_INFO = {
  regressions: { label: "Regressed", sub: "of done tasks", unit: "done tasks", rgb: "255, 122, 107", missing: "regressed" },
  footprint_misses: { label: "Footprint miss", sub: "widened mid-run", unit: "declared", rgb: "242, 178, 76", missing: "was widened onto mid-run" },
  blocked: { label: "Blocked", sub: "by a run failure", unit: "declared", rgb: "195, 155, 255", missing: "was blocked by a run failure" },
  rejected: { label: "Rejected", sub: "after a run", unit: "with a run", rgb: "154, 164, 178", missing: "was rejected after a run" },
};
const ROLLUP_NOUNS = { file: "File", dir: "Directory", crate: "Crate" };
const WINDOW_PHRASES = { "7d": "in the last 7 days", "30d": "in the last 30 days", "90d": "in the last 90 days", all: "in all recorded history" };
const PURPOSES = {
  hotspots: "Paths that are costly for agents to change. Rates are over tasks that declared the path, which is not always the landed diff.",
  coupling: "Files that are reserved together, and files that make tasks wait for a lock.",
  file: "",
  marks: "Whether regression-risk marks in implement and review prompts lowered regressions on the marked paths.",
};

let codebaseQuery = { ...DEFAULT_QUERY };
// The open view and, for the file view, its path.
let route = { view: "hotspots", path: null };
// What the reader picked on the page: every ranked path or the top of each
// signal, the path search, whether release files are grouped, the lock sort
// (`wait` or `rate`), whether every regressed task is listed, and which path
// was last copied.
let view = { showAll: false, search: "", groupRelease: true, lockSort: "wait", allTasks: false, copied: null };
let lastData = null;
let lastPayloadKey = null;
let paintedView = null;
// The file view's related-files request: the key it answers, its response or
// error, and a counter that retires superseded requests.
let related = { key: null, data: null, error: null, loading: false, request: 0 };
let requestHashChange = () => {};

function resetRelated() {
  related = { key: null, data: null, error: null, loading: false, request: related.request + 1 };
}

onWorkspaceChange(() => {
  lastData = null;
  lastPayloadKey = null;
  resetRelated();
  view = { ...view, search: "", showAll: false };
  if ($("codebase-path-search")) $("codebase-path-search").value = "";
});

// An invalid value falls back to `fallback`: the default for a hash, the
// current selection for a control edit.
function normalizedQuery(raw, fallback = DEFAULT_QUERY) {
  const window = WINDOWS.includes(raw.window) ? raw.window : fallback.window;
  const support = Number(raw.min_support);
  const min_support = Number.isSafeInteger(support) && support >= 1 ? support : fallback.min_support;
  const rollup = ROLLUPS.includes(raw.rollup) ? raw.rollup : fallback.rollup;
  const query = { window, min_support, rollup };
  // An omitted or cleared filter keeps every value; an unknown one falls back.
  for (const { key, values } of REGRESSION_DIMENSIONS) {
    const value = raw[key];
    query[key] = value == null || value === "" ? null : values.includes(value) ? value : (fallback[key] ?? null);
  }
  return query;
}

function activeFilters(query = codebaseQuery) {
  return REGRESSION_DIMENSIONS.filter((dimension) => query[dimension.key]);
}

function normalizedRoute(viewName, path) {
  if (viewName === "file") return path ? { view: "file", path } : { view: "hotspots", path: null };
  return { view: VIEWS.includes(viewName) ? viewName : "hotspots", path: null };
}

function setRoute(next) {
  if (next.path !== route.path) view = { ...view, allTasks: false, copied: null };
  route = next;
}

/// Read the view and its selection from an `#analytics/codebase[/<view>]?…`
/// hash: `viewName` is the segment after `codebase`.
function applyCodebaseHashQuery(query, viewName = null) {
  const next = normalizedQuery({
    window: query.get("window"),
    min_support: query.get("min_support") ?? DEFAULT_QUERY.min_support,
    rollup: query.get("rollup"),
    ...Object.fromEntries(REGRESSION_DIMENSIONS.map(({ key }) => [key, query.get(key)])),
  });
  const sameQuery = JSON.stringify(next) === JSON.stringify(codebaseQuery);
  codebaseQuery = next;
  setRoute(normalizedRoute(viewName, query.get("path")));
  syncCodebaseControls();
  // A view change alone answers from the payload on hand.
  if (sameQuery && lastData) {
    paint();
    ensureRelated();
  }
}

/// The hash of `target` (the open view by default) with the non-default parts
/// of the selection, so a reload or a shared link restores the same view.
function buildCodebaseHash(target = route) {
  const { view: viewName, path } = normalizedRoute(target.view, target.path);
  const sp = new URLSearchParams();
  if (codebaseQuery.window !== DEFAULT_QUERY.window) sp.set("window", codebaseQuery.window);
  if (codebaseQuery.min_support !== DEFAULT_QUERY.min_support) sp.set("min_support", String(codebaseQuery.min_support));
  if (codebaseQuery.rollup !== DEFAULT_QUERY.rollup) sp.set("rollup", codebaseQuery.rollup);
  for (const { key } of activeFilters()) sp.set(key, codebaseQuery[key]);
  if (viewName === "file") sp.set("path", path);
  const qs = sp.toString();
  return `#analytics/codebase${viewName === "hotspots" ? "" : `/${viewName}`}${qs ? `?${qs}` : ""}`;
}

function getCodebaseQuery() {
  return { ...codebaseQuery };
}

function getCodebaseRoute() {
  return { ...route };
}

function segmentButtons(id) {
  const group = $(id);
  return group ? Array.from(group.querySelectorAll("button")) : [];
}

function syncCodebaseControls() {
  for (const [id, value] of [["codebase-window", codebaseQuery.window], ["codebase-rollup", codebaseQuery.rollup]]) {
    for (const button of segmentButtons(id)) {
      const on = button.dataset.value === value;
      button.classList.toggle("on", on);
      button.setAttribute("aria-pressed", String(on));
    }
  }
  const support = $("codebase-min-support");
  if (support) {
    const value = String(codebaseQuery.min_support);
    const steps = [...new Set([...SUPPORT_STEPS, codebaseQuery.min_support])].sort((a, b) => a - b);
    if (support.dataset.steps !== steps.join(",")) {
      support.textContent = "";
      for (const step of steps) {
        const option = el("option", { text: String(step) });
        option.value = String(step);
        support.appendChild(option);
      }
      support.dataset.steps = steps.join(",");
    }
    support.value = value;
  }
  // The file and marks views have their own heading and no toolbar; Coupling
  // keeps the window, rollup and minimum but has no path search.
  const toolbar = $("codebase-toolbar");
  if (toolbar) toolbar.hidden = route.view === "file" || route.view === "marks";
  const search = $("codebase-search");
  if (search) search.hidden = route.view !== "hotspots";
  const views = $("codebase-views");
  if (views) {
    views.hidden = route.view === "file" || route.view === "marks";
    for (const link of views.querySelectorAll("a")) {
      const on = link.dataset.view === route.view;
      link.classList.toggle("on", on);
      if (on) link.setAttribute("aria-current", "page");
      else link.setAttribute("aria-current", "false");
      link.setAttribute("href", scopedHashHref(buildCodebaseHash({ view: link.dataset.view })));
    }
  }
  const purpose = $("codebase-purpose");
  if (purpose) {
    purpose.textContent = PURPOSES[route.view] || "";
    purpose.hidden = !purpose.textContent;
  }
}

/// Change the selection and ask for a new hash; an invalid or unchanged
/// selection asks for nothing.
function updateQuery(update) {
  const next = normalizedQuery({ ...codebaseQuery, ...update }, codebaseQuery);
  if (JSON.stringify(next) === JSON.stringify(codebaseQuery)) {
    syncCodebaseControls();
    return;
  }
  codebaseQuery = next;
  syncCodebaseControls();
  requestHashChange();
}

/// Open another view (and path) from the payload on hand, then ask for its hash.
function navigate(target) {
  const next = normalizedRoute(target.view, target.path);
  if (next.view === route.view && next.path === route.path) return;
  setRoute(next);
  syncCodebaseControls();
  paint();
  ensureRelated();
  requestHashChange();
}

/// Bind the window and group-by segments, the minimum-support select, the
/// path search and the view tabs. `onChange` runs after the selection or the
/// view changes, to rewrite the hash.
function wireCodebaseControls(onChange) {
  requestHashChange = onChange;
  for (const [id, field] of [["codebase-window", "window"], ["codebase-rollup", "rollup"]]) {
    for (const button of segmentButtons(id)) {
      button.addEventListener("click", () => updateQuery({ [field]: button.dataset.value }));
    }
  }
  const support = $("codebase-min-support");
  if (support) support.addEventListener("change", () => updateQuery({ min_support: Number(support.value) }));
  const search = $("codebase-path-search");
  if (search) {
    search.addEventListener("input", () => {
      view = { ...view, search: search.value };
      paint();
    });
  }
  const views = $("codebase-views");
  if (views) {
    for (const link of views.querySelectorAll("a")) {
      link.addEventListener("click", (event) => followLink(event, { view: link.dataset.view }));
    }
  }
  syncCodebaseControls();
}

function analyticsPath() {
  const sp = new URLSearchParams();
  sp.set("window", codebaseQuery.window);
  sp.set("min_support", String(codebaseQuery.min_support));
  sp.set("rollup", codebaseQuery.rollup);
  for (const { key } of activeFilters()) sp.set(key, codebaseQuery[key]);
  return `/api/codebase/analytics?${sp.toString()}`;
}

function setMeta(text) {
  const meta = $("codebase-meta");
  if (meta) meta.textContent = text;
}

function fetchAndRenderCodebase() {
  if (isAggregateView()) {
    lastData = null;
    lastPayloadKey = null;
    paintedView = null;
    resetRelated();
    setMeta("");
    renderPanelPlaceholder(BODY_ID);
    return Promise.resolve();
  }
  const path = analyticsPath();
  return requestPanel(BODY_ID, path, () => fetchJson(path), (data) => renderCodebase(data));
}

// ---- figures ----------------------------------------------------------------

function pct(rate) {
  const value = Number(rate);
  if (!Number.isFinite(value)) return "—";
  return `${(value * 100).toFixed(value > 0 && value < 0.1 ? 1 : 0)}%`;
}

function count(value) {
  return (Number(value) || 0).toLocaleString("en-US");
}

function plural(n, one, many = `${one}s`) {
  return `${count(n)} ${Number(n) === 1 ? one : many}`;
}

function section(data, key) {
  return (data && data[key]) || {};
}

function rowsOf(data, key) {
  return Array.isArray(section(data, key).rows) ? section(data, key).rows : [];
}

// A wait as the panel reads it: minutes under an hour, then hours and
// minutes, then days.
function waitDuration(ms) {
  const minutes = Math.round((Number(ms) || 0) / 60000);
  if (minutes < 1) return "<1 min";
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  if (hours >= 48) return `${count(Math.round(minutes / 1440))} d`;
  return minutes % 60 ? `${hours} h ${minutes % 60} min` : `${hours} h`;
}

// A directory key ends in `/`; its base name keeps the slash.
function splitPath(path) {
  const value = String(path ?? "");
  const trimmed = value.endsWith("/") ? value.slice(0, -1) : value;
  const at = trimmed.lastIndexOf("/");
  return at < 0 ? { dir: "", base: value } : { dir: value.slice(0, at + 1), base: value.slice(at + 1) };
}

// One directory and its own `tests/` child move together by design.
function expectedPair(paths) {
  const [left, right] = paths || [];
  if (typeof left !== "string" || typeof right !== "string") return false;
  if (!left.endsWith("/") || !right.endsWith("/")) return false;
  return right === `${left}tests/` || left === `${right}tests/`;
}

function releasePair(paths) {
  return Array.isArray(paths) && paths.length === 2 && paths.every((path) => RELEASE_FILES.includes(path));
}

// ---- model ------------------------------------------------------------------

// Whether a signal has anything to show in this window. Regressed stays while
// a filter is set, so the filter row can say what it left.
function signalShown(data, key) {
  if (rowsOf(data, key).length) return true;
  return key === "regressions" && activeFilters(data.regression_filter || {}).length > 0;
}

/// Everything the views read, derived from one payload and the view state.
function buildModel(data) {
  const signals = SIGNALS.filter((key) => signalShown(data, key));
  // path -> { signal: { rank, row } } over the shown signals.
  const index = new Map();
  for (const key of signals) {
    rowsOf(data, key).forEach((row, at) => {
      if (!index.has(row.path)) index.set(row.path, {});
      index.get(row.path)[key] = { rank: at + 1, row };
    });
  }
  // Paths under two or more signals, ordered by the server (the same list
  // `orbit codebase analytics` prints): by how many, then by the highest rate
  // outside Rejected (whose one-off rejections would otherwise lead).
  const multi = rowsOf(data, "overlap")
    .filter((row) => index.has(row.path) && Object.keys(index.get(row.path)).length >= 2)
    .map((row) => row.path);
  const flagged = new Set(multi);
  // Every other ranked path, by signal, in that signal's order.
  const singles = signals.map((key) => ({
    key,
    paths: rowsOf(data, key).map((row) => row.path).filter((path) => !flagged.has(path)),
  }));
  // How many ranked regression paths each task regressed.
  const regressedPaths = new Map();
  for (const row of rowsOf(data, "regressions")) {
    for (const id of new Set((row.regressed_tasks || []).map((task) => task.task_id))) {
      regressedPaths.set(id, (regressedPaths.get(id) || 0) + 1);
    }
  }
  const limit = Number(data.row_limit) || Math.max(0, ...SIGNALS.map((key) => rowsOf(data, key).length));
  return { data, signals, index, multi, singles, regressedPaths, limit };
}

function causeTotals(data) {
  const blocked = section(data, "blocked");
  const causes = Array.isArray(blocked.causes) ? blocked.causes : [];
  const byKind = (kind) => causes.filter((cause) => cause.kind === kind).reduce((sum, cause) => sum + (Number(cause.tasks) || 0), 0);
  return { infrastructure: byKind("infrastructure"), manual: byKind("manual"), unclassified: byKind("unclassified") };
}

// Whether the payload carries lock waits. An older one does not, and the
// view renders rates only.
function lockWaitsRecorded(data) {
  return section(data, "lock_contention").wait_time_recorded === true;
}

// The supported rows ranked by total wait; empty when the payload records no
// waits or no task waited, which also hides the sort.
function rowsByWait(data) {
  const rows = section(data, "lock_contention").rows_by_wait;
  return lockWaitsRecorded(data) && Array.isArray(rows) ? rows : [];
}

function lockByWait(data) {
  return view.lockSort === "wait" && rowsByWait(data).length > 0;
}

// Nothing to rank and nothing below support: no requester was ever refused.
function lockIsQuiet(data) {
  const lock = section(data, "lock_contention");
  return rowsOf(data, "lock_contention").length === 0 && !Number(lock.below_support) && !Number(lock.denials);
}

// ---- shared nodes -----------------------------------------------------------

function pathNode(path, extraClass = "") {
  const { dir, base } = splitPath(path);
  return el("span", { class: `codebase-path${extraClass ? ` ${extraClass}` : ""}` }, [
    el("span", { class: "codebase-path-dir", text: dir }),
    el("span", { class: "codebase-path-base", text: base }),
  ]);
}

function rateBar(fraction) {
  const width = Math.max(2, Math.min(100, Math.round((Number(fraction) || 0) * 100)));
  return el("span", { class: "codebase-bar", "aria-hidden": "true" }, [el("span", { style: { width: `${width}%` } })]);
}

function note(text, extraClass = "") {
  return el("p", { class: `codebase-note${extraClass ? ` ${extraClass}` : ""}`, text });
}

function actionButton(label, onClick, extraClass = "") {
  const button = el("button", { class: `codebase-btn${extraClass ? ` ${extraClass}` : ""}`, type: "button", text: label });
  button.addEventListener("click", onClick);
  return button;
}

function keyed(node, key, hash) {
  node.dataset.key = key;
  node.dataset.hash = JSON.stringify(hash);
  return node;
}

function signalDot(key) {
  const dot = el("span", { class: "codebase-dot", "aria-hidden": "true" });
  dot.dataset.signal = key;
  return dot;
}

// A plain click opens the view in place; a modified click (new tab, copy
// link) follows the scoped href.
function followLink(event, target) {
  if (event && (event.defaultPrevented || event.button > 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey)) return;
  if (event && typeof event.preventDefault === "function") event.preventDefault();
  navigate(target);
}

function viewLink(target, children, extraClass = "") {
  const link = el("a", { class: extraClass }, children);
  link.setAttribute("href", scopedHashHref(buildCodebaseHash(target)));
  link.addEventListener("click", (event) => followLink(event, target));
  return link;
}

function fileLink(path) {
  const link = viewLink({ view: "file", path }, [pathNode(path)], "codebase-path-link");
  link.dataset.path = path;
  return link;
}

function backLink() {
  return viewLink({ view: "hotspots" }, ["‹ Hotspots"], "codebase-back");
}

function lowerSupportButton(data, label) {
  if (!(Number(data.min_support) > LOWERED_SUPPORT)) return null;
  return actionButton(label, () => updateQuery({ min_support: LOWERED_SUPPORT }));
}

// ---- hotspots ---------------------------------------------------------------

function clearFiltersButton() {
  return actionButton("Clear filters", () => updateQuery(DEFAULT_FILTER), "small");
}

/// One select per dimension, every value some link takes (and the chosen
/// one) with its link count. They filter the Regressed column.
function filtersNode(model) {
  const { data } = model;
  const regressions = section(data, "regressions");
  const breakdowns = regressions.breakdowns || {};
  const filter = data.regression_filter || {};
  const block = el("div", { class: "codebase-filters", role: "group", "aria-label": "Filter regressions" }, [
    el("span", { class: "codebase-filters-label", text: "Regressions only:" }),
  ]);
  if (!Number(regressions.links) && !activeFilters(filter).length) {
    block.appendChild(el("span", { class: "codebase-extra", text: "no regression link targets a task done in this window" }));
    return keyed(block, "filters", { links: 0 });
  }
  const selects = [];
  for (const dimension of REGRESSION_DIMENSIONS) {
    const entries = Array.isArray(breakdowns[dimension.key]) ? breakdowns[dimension.key] : [];
    const shown = entries.filter((entry) => Number(entry.links) > 0 || entry.value === filter[dimension.key]);
    const control = el("select", { class: "codebase-select" });
    control.dataset.dimension = dimension.key;
    const any = el("option", { text: "Any" });
    any.value = "";
    control.appendChild(any);
    for (const entry of shown) {
      const option = el("option", { text: `${valueLabel(entry.value)} · ${count(entry.links)}` });
      option.value = entry.value;
      if (entry.value === filter[dimension.key]) option.selected = true;
      control.appendChild(option);
    }
    control.value = filter[dimension.key] || "";
    control.addEventListener("change", () => updateQuery({ [dimension.key]: control.value || null }));
    selects.push(shown.map((entry) => [entry.value, entry.links]));
    block.appendChild(el("label", { class: "codebase-filter" }, [dimension.label, control]));
  }
  const active = activeFilters(filter);
  if (active.length) {
    block.appendChild(el("span", { class: "codebase-extra codebase-filter-count", text: `${count(regressions.matching_links)} of ${plural(regressions.links, "regression link")} on ${plural(regressions.regressed, "regressed task")}` }));
    block.appendChild(clearFiltersButton());
  }
  return keyed(block, "filters", { selects, filter, links: regressions.links, matching: regressions.matching_links, regressed: regressions.regressed });
}

function cellNode(key, hit, total) {
  const cell = el("td", { class: "codebase-cell-td" });
  cell.dataset.signal = key;
  if (!hit) {
    cell.appendChild(el("span", { class: "codebase-cell empty", text: "—" }));
    return cell;
  }
  const { row, rank } = hit;
  const rate = Math.max(0, Math.min(1, Number(row.rate) || 0));
  const shade = el("span", {
    class: "codebase-cell",
    title: `${SIGNAL_INFO[key].label}: ${count(row.numerator)} of ${count(row.denominator)} ${SIGNAL_INFO[key].unit} · #${rank} of ${count(total)}`,
    style: { background: `rgba(${SIGNAL_INFO[key].rgb}, ${(0.08 + rate * 0.37).toFixed(2)})` },
  }, [
    el("span", { class: "codebase-cell-rate", text: pct(row.rate) }),
    el("span", { class: "codebase-cell-frac", text: `${count(row.numerator)}/${count(row.denominator)}` }),
  ]);
  cell.appendChild(shade);
  return cell;
}

function hotRow(model, path) {
  const hits = model.index.get(path) || {};
  const row = el("tr", { class: "codebase-hot-row" }, [
    el("td", { class: "codebase-hot-path" }, [fileLink(path)]),
    ...model.signals.map((key) => cellNode(key, hits[key], rowsOf(model.data, key).length)),
  ]);
  row.dataset.path = path;
  return row;
}

function sectionRow(model, text, kind) {
  const cell = el("td", { class: "codebase-hot-section", text });
  cell.setAttribute("colspan", String(model.signals.length + 1));
  const row = el("tr", {}, [cell]);
  row.dataset.section = kind;
  return row;
}

/// The paths each section lists: under a search, every ranked match; else
/// the flagged paths, then the top of each signal or, with Show all, the rest.
function hotspotSections(model) {
  const query = view.search.trim().toLowerCase();
  const matches = (path) => !query || path.toLowerCase().includes(query);
  const multi = model.multi.filter(matches);
  const seen = new Set(multi);
  const singles = [];
  for (const { paths } of model.singles) {
    const kept = paths.filter(matches).filter((path) => !seen.has(path));
    for (const path of query || view.showAll ? kept : kept.slice(0, SINGLES_SHOWN)) {
      seen.add(path);
      singles.push(path);
    }
  }
  return { query, multi, singles };
}

function hotspotTable(model) {
  const { data, signals, index } = model;
  const box = el("section", { class: "codebase-hot", "aria-label": "Hotspots" });
  if (!signals.length) {
    box.appendChild(el("div", { class: "codebase-empty-state" }, [
      el("p", { class: "codebase-empty-title", text: `No path reaches minimum support ${count(data.min_support)} under any signal in this window.` }),
      el("div", { class: "codebase-actions" }, [lowerSupportButton(data, `Show paths with ${LOWERED_SUPPORT}+ tasks`), data.window !== "all" ? actionButton("Use all time", () => updateQuery({ window: "all" })) : null]),
    ]));
    return keyed(box, "hotspots", { empty: true, min: data.min_support, window: data.window });
  }
  const { query, multi, singles } = hotspotSections(model);
  const head = el("tr", {}, [
    el("th", { class: "codebase-hot-path", text: ROLLUP_NOUNS[data.rollup] || "Path" }),
    ...signals.map((key) => {
      const cell = el("th", { class: "codebase-hot-signal" }, [
        el("span", { class: "codebase-hot-label" }, [signalDot(key), SIGNAL_INFO[key].label]),
        el("span", { class: "codebase-hot-sub", text: SIGNAL_INFO[key].sub }),
      ]);
      cell.dataset.signal = key;
      return cell;
    }),
  ]);
  for (const cell of head.children) cell.setAttribute("scope", "col");
  const body = el("tbody");
  if (multi.length) {
    body.appendChild(sectionRow(model, `Flagged by two or more signals · ${count(multi.length)}`, "multi"));
    for (const path of multi) body.appendChild(hotRow(model, path));
  }
  if (singles.length) {
    const label = query ? `One signal · ${count(singles.length)}` : view.showAll ? `One signal · ${count(singles.length)}` : `One signal · top ${SINGLES_SHOWN} of each`;
    body.appendChild(sectionRow(model, label, "single"));
    for (const path of singles) body.appendChild(hotRow(model, path));
  }
  if (!multi.length && !singles.length) {
    const cell = el("td", { class: "codebase-empty", text: query ? `No ranked path matches “${view.search.trim()}”.` : "No path is ranked in this window." });
    cell.setAttribute("colspan", String(signals.length + 1));
    body.appendChild(el("tr", {}, [cell]));
  }
  const table = el("table", { class: "codebase-hot-table" }, [el("thead", {}, [head]), body]);
  table.setAttribute("aria-label", "Paths by signal");
  // The table scrolls sideways inside its box on a narrow screen.
  const scroll = el("div", { class: "codebase-hot-scroll", role: "region", "aria-label": "Paths by signal" }, [table]);
  scroll.tabIndex = 0;
  box.appendChild(scroll);
  const shown = multi.length + singles.length;
  const foot = el("div", { class: "codebase-hot-foot" }, [
    el("span", { class: "codebase-extra", text: query ? `${count(shown)} of ${plural(index.size, "ranked path")} match` : `${count(shown)} of ${plural(index.size, "ranked path")} shown` }),
  ]);
  if (!query && (view.showAll || shown < index.size)) {
    const toggle = actionButton(view.showAll ? `Show top ${SINGLES_SHOWN} of each` : `Show all ${count(index.size)}`, () => {
      view = { ...view, showAll: !view.showAll };
      paint();
    }, "small");
    toggle.setAttribute("aria-expanded", String(view.showAll));
    foot.appendChild(toggle);
  }
  box.appendChild(foot);
  if (query) {
    // Paths no signal ranks can still be opened: their files and partners.
    const ranked = new Set(index.keys());
    const others = (Array.isArray(data.known_paths) ? data.known_paths : [])
      .filter((path) => !ranked.has(path) && path.toLowerCase().includes(query)).slice(0, OTHER_PATHS_SHOWN);
    if (others.length) {
      const list = el("ul", { class: "codebase-other-paths", "aria-label": "Other matching paths" });
      for (const path of others) list.appendChild(el("li", {}, [fileLink(path)]));
      box.appendChild(el("div", { class: "codebase-others" }, [el("p", { class: "codebase-eyebrow", text: "Not ranked · open to see what changes with it" }), list]));
    }
  }
  const cells = [...multi, ...singles].map((path) => signals.map((key) => {
    const hit = index.get(path)?.[key];
    return hit ? [hit.rank, hit.row.rate, hit.row.numerator, hit.row.denominator] : null;
  }));
  return keyed(box, "hotspots", {
    signals, rollup: data.rollup, multi, singles, cells, total: index.size, showAll: view.showAll, query,
    lengths: signals.map((key) => rowsOf(data, key).length), known: query ? data.known_paths || null : null,
  });
}

function linkTo(text, target) {
  return viewLink(target, [text]);
}

function riskMarksLine(data) {
  const marks = section(data, "regressions").risk_marks || {};
  if (!marks.enabled_at) return ["No implement or review prompt has marked a regression-prone file yet, so there is nothing to compare."];
  const sample = (rates) => Number(rates?.denominator)
    ? `${pct(rates.rate)} (${count(rates.numerator)} of ${count(rates.denominator)})`
    : "no tasks";
  return [
    `Regression-risk marks: ${sample(marks.with_marks)} of marked tasks regressed, against ${sample(marks.before)} in the ${count(marks.window_days)} days before marking. `,
    linkTo("Per-path comparison", { view: "marks" }),
  ];
}

/// The footnotes that stand in for whole sections: hidden columns, blocks
/// that never count, the regression-risk marks and where defect trends live.
function notesNode(model) {
  const { data, signals } = model;
  const lines = [];
  for (const key of SIGNALS.filter((signal) => !signals.includes(signal))) {
    const text = key === "rejected" && !Number(section(data, "rejected").rejected_with_run)
      ? "No task was rejected after a run in this window, so that column is hidden."
      : `No path with ${count(data.min_support)}+ tasks ${SIGNAL_INFO[key].missing} in this window, so the ${SIGNAL_INFO[key].label} column is hidden.`;
    lines.push([text]);
  }
  const causes = causeTotals(data);
  let blocks = `Blocks from infrastructure (${count(causes.infrastructure)}) and manual holds (${count(causes.manual)}) never count against paths.`;
  if (causes.unclassified > 0) blocks += ` Neither do ${plural(causes.unclassified, "block")} with no recorded cause.`;
  lines.push([blocks]);
  if (data.rollup === "crate") {
    lines.push(["At crate grouping nearly every crate clears the minimum, so most crates turn up under several signals. Group by file or directory to find hotspots."]);
  }
  lines.push(riskMarksLine(data));
  const defects = el("a", { text: "Metrics › Defects" });
  defects.setAttribute("href", scopedHashHref("#analytics/metrics"));
  lines.push(["Defect trends are on ", defects, "."]);
  const node = el("div", { class: "codebase-notes" }, lines.map((parts) => el("p", { class: "codebase-note" }, parts)));
  const marks = section(data, "regressions").risk_marks || {};
  return keyed(node, "notes", {
    hidden: SIGNALS.filter((signal) => !signals.includes(signal)), rejected: section(data, "rejected").rejected_with_run,
    causes, rollup: data.rollup, min: data.min_support,
    marks: [marks.enabled_at, marks.window_days, marks.with_marks, marks.before], links: [scopedHashHref(buildCodebaseHash({ view: "marks" })), scopedHashHref("#analytics/metrics")],
  });
}

function hotspotNodes(model) {
  return [filtersNode(model), hotspotTable(model), notesNode(model)];
}

// ---- file -------------------------------------------------------------------

function copyPath(path) {
  const clipboard = globalThis.navigator && globalThis.navigator.clipboard;
  if (!clipboard || typeof clipboard.writeText !== "function") return;
  clipboard.writeText(path).then(() => {
    view = { ...view, copied: path };
    paint();
  }, () => {});
}

function fileHead(model, path) {
  const { dir, base } = splitPath(path);
  const head = el("div", { class: "codebase-file-head" }, [
    backLink(),
    el("div", { class: "codebase-file-title" }, [
      el("div", { class: "codebase-file-name" }, [
        el("span", { class: "codebase-eyebrow", text: ROLLUP_NOUNS[model.data.rollup] || "Path" }),
        el("span", { class: "codebase-path-dir codebase-file-dir", text: dir }),
        el("h3", { id: "codebase-file-path", class: "codebase-path-base codebase-file-base", text: base }),
      ]),
      el("div", { class: "codebase-actions" }, [actionButton(view.copied === path ? "Copied" : "Copy path", () => copyPath(path), "small")]),
    ]),
  ]);
  return keyed(head, "file-head", { path, rollup: model.data.rollup, copied: view.copied === path, back: buildCodebaseHash({ view: "hotspots" }) });
}

function fileTiles(model, path) {
  const { data, signals, index, limit } = model;
  const hits = index.get(path) || {};
  const node = el("section", { class: "codebase-file-tiles", "aria-label": "Signals" });
  const keys = signals.length ? signals : ["regressions", "footprint_misses", "blocked"];
  for (const key of keys) {
    const hit = hits[key];
    const tile = el("div", { class: `codebase-file-tile${hit ? "" : " absent"}` }, [
      el("span", { class: "codebase-tile-label" }, [signalDot(key), SIGNAL_INFO[key].label]),
      el("span", { class: "codebase-tile-big", text: hit ? pct(hit.row.rate) : "—" }),
      el("span", { class: "codebase-tile-sub" }, hit
        ? [`${count(hit.row.numerator)} of ${count(hit.row.denominator)} ${SIGNAL_INFO[key].unit} · `, el("span", { class: "codebase-tile-rank", text: `#${hit.rank}` }), ` of ${count(rowsOf(data, key).length)}`]
        : [`not in the top ${count(limit)}`]),
    ]);
    tile.dataset.signal = key;
    node.appendChild(tile);
  }
  return keyed(node, "file-tiles", { path, keys, hits, limit, lengths: keys.map((key) => rowsOf(data, key).length) });
}

function regressedTaskNode(task, regressedPaths) {
  const wide = regressedPaths.get(task.task_id) || 0;
  const link = el("a", { class: "codebase-task-link", text: task.task_id });
  link.setAttribute("href", taskHref(task.task_id));
  const top = el("div", { class: "codebase-task-top" }, [
    link,
    el("span", { class: "codebase-crew", text: task.crew || "unassigned" }),
    el("span", { class: "codebase-task-title", text: task.title || "" }),
  ]);
  if (wide >= WIDE_TASK_PATHS) {
    top.appendChild(el("span", { class: "codebase-tag wide", text: `${count(wide)} ranked paths`, title: "This task regressed several ranked paths at once" }));
  }
  const item = el("li", { class: "codebase-task" }, [top]);
  const links = Array.isArray(task.regressions) ? task.regressions : [];
  for (const id of task.regression_tasks || []) {
    const regression = el("a", { class: "codebase-task-link", text: id });
    regression.setAttribute("href", taskHref(id));
    const line = el("div", { class: "codebase-task-by" }, [el("span", { text: "regressed by" }), regression]);
    line.dataset.task = id;
    const found = links.find((entry) => entry.task_id === id);
    if (found) {
      for (const key of ["nature", "layer", "detected_by"]) {
        const tag = el("span", { class: "codebase-by-dim", text: valueLabel(found[key]) });
        if (key === "layer" && found.layer_source) {
          tag.title = `Layer, from its ${found.layer_source === "landed" ? "landed diff" : found.layer_source === "footprint" ? "declared footprint" : "missing paths"}`;
        }
        tag.dataset.dimension = key;
        tag.dataset.value = found[key];
        line.appendChild(tag);
      }
    }
    item.appendChild(line);
  }
  item.dataset.task = task.task_id;
  if (wide >= WIDE_TASK_PATHS) item.dataset.wide = String(wide);
  return item;
}

function fileTasks(model, path) {
  const hit = model.index.get(path)?.regressions;
  const tasks = hit ? hit.row.regressed_tasks || [] : [];
  const head = el("div", { class: "codebase-box-head" }, [el("h4", { text: "Regressed tasks" })]);
  if (tasks.length > TASKS_SHOWN) {
    const toggle = actionButton(view.allTasks ? "Show fewer" : `All ${count(tasks.length)}`, () => {
      view = { ...view, allTasks: !view.allTasks };
      paint();
    }, "link");
    toggle.setAttribute("aria-expanded", String(view.allTasks));
    head.appendChild(toggle);
  }
  const box = el("section", { class: "codebase-box codebase-file-tasks", "aria-label": "Regressed tasks" }, [head]);
  const filters = activeFilters(model.data.regression_filter || {});
  if (filters.length) {
    box.appendChild(note(`Filtered to ${filters.map(({ label, key }) => `${label.toLowerCase()} ${valueLabel(model.data.regression_filter[key])}`).join(", ")}: only matching regressions are counted and listed.`, "codebase-filter-note"));
  }
  if (!hit) {
    box.appendChild(note(`Not in the top ${count(model.limit)} regressed paths, so no regressed tasks are listed.`, "codebase-box-note"));
  } else {
    const shown = view.allTasks ? tasks : tasks.slice(0, TASKS_SHOWN);
    const list = el("ul", { class: "codebase-tasks" });
    for (const task of shown) list.appendChild(regressedTaskNode(task, model.regressedPaths));
    box.appendChild(list);
    if (Number(hit.row.numerator) > tasks.length) {
      box.appendChild(note(tasks.length === 1
        ? `Only the most recently done of ${count(hit.row.numerator)} regressed tasks is listed.`
        : `The ${count(tasks.length)} most recently done of ${count(hit.row.numerator)} regressed tasks are listed.`, "codebase-box-note"));
    }
    if (shown.some((task) => (model.regressedPaths.get(task.task_id) || 0) >= WIDE_TASK_PATHS)) {
      box.appendChild(note("A task marked with a path count regressed that many ranked paths at once, so a single fix there can lift several rows.", "codebase-box-note"));
    }
  }
  return keyed(box, "file-tasks", {
    path, filter: model.data.regression_filter || null, limit: model.limit, allTasks: view.allTasks, row: hit ? hit.row : null,
    wide: tasks.map((task) => model.regressedPaths.get(task.task_id) || 0),
  });
}

// Why no related file is listed, from the response's empty reason, with a
// wider window or a lower minimum that can list one.
function relatedEmpty(data) {
  const reason = data.empty_reason || {};
  const when = WINDOW_PHRASES[codebaseQuery.window] || "in this window";
  const actions = [];
  const lower = (support) => {
    if (Number(support) >= 1 && Number(support) < Number(codebaseQuery.min_support)) {
      actions.push(actionButton(`Lower minimum to ${count(support)}`, () => updateQuery({ min_support: Number(support) })));
    }
  };
  const wider = () => { if (codebaseQuery.window !== "all") actions.push(actionButton("Use all time", () => updateQuery({ window: "all" }))); };
  let text;
  switch (reason.kind) {
    case "no_landed_change":
      text = `No landed task with a recorded landing changed this file ${when}.`;
      wider();
      break;
    case "path_below_minimum":
      text = `Only ${plural(reason.path_support, "landed task")} changed this file ${when}; ranking partners needs ${count(reason.min_support ?? data.min_support)}.`;
      wider();
      lower(reason.path_support);
      break;
    case "no_partner":
      text = `Every landed task that changed this file ${when} changed it alone.`;
      wider();
      break;
    case "partners_below_minimum":
      text = `${plural(reason.partners, "file")} changed alongside it ${when}, but none in ${count(reason.min_support)} or more landed tasks; the most shared is ${count(reason.max_shared)}.`;
      lower(reason.max_shared);
      wider();
      break;
    default:
      text = `No file changed alongside it in ${count(codebaseQuery.min_support)} or more landed tasks ${when}.`;
      wider();
  }
  return [el("p", { class: "codebase-empty", text }), actions.length ? el("div", { class: "codebase-actions" }, actions) : null];
}

function relatedRow(row) {
  const support = row.support || {};
  const item = el("li", { class: "codebase-related-row" }, [
    el("div", { class: "codebase-related-top" }, [
      fileLink(row.path),
      el("span", { class: "codebase-related-rate" }, [
        el("span", { class: "codebase-rate", text: pct(row.jaccard) }),
        el("span", { class: "codebase-frac", text: `${count(support.both)}/${count(support.either)}` }),
      ]),
    ]),
    rateBar(row.jaccard),
  ]);
  item.dataset.path = row.path;
  const tasks = Array.isArray(row.tasks) ? row.tasks : [];
  if (tasks.length) {
    const evidence = el("details", { class: "codebase-related-evidence" }, [el("summary", { text: `Changed together in ${plural(tasks.length, "task")}` })]);
    for (const task of tasks) {
      const link = el("a", { class: "codebase-task-link", text: task.title || task.task_id, title: task.commit || "" });
      link.setAttribute("href", taskHref(task.task_id));
      evidence.appendChild(link);
    }
    item.appendChild(evidence);
  }
  return item;
}

function fileRelated(path) {
  const box = el("section", { class: "codebase-box codebase-file-related", "aria-label": "Changes with" }, [
    el("div", { class: "codebase-box-head stacked" }, [el("h4", { text: "Changes with" }), el("span", { class: "codebase-extra", text: "files landed in the same tasks" })]),
  ]);
  const body = el("div", { class: "codebase-box-body" });
  box.appendChild(body);
  const done = (hash) => keyed(box, "file-related", hash);
  if (path.endsWith("/")) {
    body.appendChild(el("p", { class: "codebase-empty", text: "Changes with pairs single files. Group by file to see what lands alongside one." }));
    body.appendChild(el("div", { class: "codebase-actions" }, [actionButton("Group by file", () => updateQuery({ rollup: "file" }))]));
    return done({ path, rollup: true });
  }
  if (related.error) {
    body.appendChild(el("p", { class: "codebase-empty", text: `Could not load related files: ${related.error}` }));
    return done({ path, error: related.error });
  }
  const data = related.data;
  if (!data) {
    body.appendChild(el("p", { class: "codebase-empty", text: "Loading related files…" }));
    return done({ path, loading: true });
  }
  const rows = Array.isArray(data.rows) ? data.rows : [];
  if (rows.length) {
    const list = el("ul", { class: "codebase-related-list" });
    for (const row of rows) list.appendChild(relatedRow(row));
    body.appendChild(list);
  } else {
    for (const node of relatedEmpty(data)) if (node) body.appendChild(node);
  }
  const unavailable = Number(data.unavailable_landed_tasks) || 0;
  const parts = [`${plural(data.path_support, "landed task")} changed this file`];
  if (Number(data.excluded_large)) parts.push(`${count(data.excluded_large)} bulk tasks over ${count(data.large_footprint_cap)} files left out`);
  if (unavailable) parts.push(`${plural(unavailable, "done task")} without a recorded landing, finished by hand or before Orbit recorded landings`);
  body.appendChild(note(`${parts.join(" · ")}.`, "codebase-related-note"));
  return done({ path, data, window: codebaseQuery.window, min: codebaseQuery.min_support });
}

function fileNodes(model) {
  const path = route.path;
  return [fileHead(model, path), fileTiles(model, path), fileTasks(model, path), fileRelated(path)];
}

// The related query reads only the window and minimum support; a changed
// payload refetches it too.
function relatedFetchKey(path) {
  return JSON.stringify([path, codebaseQuery.window, codebaseQuery.min_support, lastPayloadKey]);
}

function ensureRelated() {
  if (route.view !== "file" || !route.path || route.path.endsWith("/") || isAggregateView() || !lastData) return;
  const key = relatedFetchKey(route.path);
  if (key === related.key && !related.error) return;
  fetchRelated(route.path, key);
}

async function fetchRelated(path, key) {
  const request = related.request + 1;
  // Keep the last answer for the same file on screen while a refresh runs.
  const keep = related.data && related.data.path === path ? related.data : null;
  related = { key, data: keep, error: null, loading: true, request };
  paint();
  const visit = captureWorkspaceVisit();
  const sp = new URLSearchParams({ path, window: codebaseQuery.window, min_support: String(codebaseQuery.min_support), top: String(RELATED_TOP) });
  const current = () => request === related.request && visit.isCurrent() && route.view === "file" && route.path === path;
  try {
    const data = await fetchJson(`/api/codebase/related?${sp}`);
    if (!current()) return;
    related = { ...related, data: data && typeof data === "object" && !Array.isArray(data) ? data : { path, rows: [] }, loading: false };
  } catch (error) {
    if (!current()) return;
    related = { ...related, data: null, error: error.message || String(error), loading: false };
  }
  paint();
}

// ---- coupling & locks -------------------------------------------------------

function pairRow(pair) {
  const [left, right] = pair.paths || [];
  const item = el("li", { class: "codebase-pair-row" }, [
    el("div", { class: "codebase-pair-paths" }, [
      pathNode(left),
      el("span", { class: "codebase-pair-second" }, [el("span", { class: "codebase-pair-arrow", text: "↔ " }), pathNode(right)]),
      expectedPair(pair.paths) ? el("span", { class: "codebase-tag", text: "expected: its tests" }) : null,
    ]),
    el("div", { class: "codebase-pair-figure" }, [
      el("span", {}, [el("span", { class: "codebase-rate", text: pct(pair.rate) }), " ", el("span", { class: "codebase-frac", text: `${count(pair.numerator)}/${count(pair.denominator)}` })]),
      rateBar(pair.rate),
    ]),
  ]);
  item.dataset.pair = (pair.paths || []).join("\n");
  return item;
}

function releaseRow(pairs) {
  const rates = pairs.map((pair) => Number(pair.rate) || 0);
  const low = Math.min(...rates);
  const high = Math.max(...rates);
  const files = RELEASE_FILES.filter((file) => pairs.some((pair) => pair.paths.includes(file)));
  const row = el("li", { class: "codebase-release-row" }, [
    el("div", { class: "codebase-release-top" }, [
      el("span", { class: "codebase-release-title", text: `Release bump set · ${plural(files.length, "file")}` }),
      el("span", { class: "codebase-release-range", text: low === high ? pct(low) : `${pct(low)}–${pct(high)}` }),
    ]),
    el("span", { class: "codebase-release-files", text: `${files.join(" · ")} · ${plural(pairs.length, "pair")}` }),
  ]);
  row.dataset.pairs = String(pairs.length);
  return row;
}

function pairsNode(model) {
  const { data } = model;
  const coupling = section(data, "change_coupling");
  const pairs = rowsOf(data, "change_coupling");
  const head = el("div", { class: "codebase-box-head" }, [
    el("div", {}, [el("h4", { text: "Reserved together" }), el("span", { class: "codebase-extra", text: "tasks holding both ÷ tasks holding either" })]),
  ]);
  const box = el("section", { class: "codebase-box codebase-pairs", "aria-label": "Reserved together" }, [head]);
  const release = pairs.filter((pair) => releasePair(pair.paths));
  if (release.length) {
    const toggle = el("input", { type: "checkbox", class: "codebase-check" });
    toggle.checked = view.groupRelease;
    toggle.addEventListener("change", () => {
      view = { ...view, groupRelease: toggle.checked };
      paint();
    });
    head.appendChild(el("label", { class: "codebase-toggle" }, [toggle, "Group release files"]));
  }
  const below = Number(coupling.below_support) || 0;
  const large = Number(coupling.excluded_large) || 0;
  const largeNote = large > 0 ? `${plural(large, "reservation")} naming more than ${count(coupling.large_footprint_cap)} files ${large === 1 ? "is" : "are"} left out of every pair.` : "";
  if (!pairs.length) {
    const nouns = { file: "files", dir: "directories", crate: "crates" };
    const actions = [];
    if (data.rollup !== "dir") actions.push(actionButton("Group by directory", () => updateQuery({ rollup: "dir" })));
    const lower = lowerSupportButton(data, `Lower to ${LOWERED_SUPPORT}`);
    if (lower) actions.push(lower);
    box.appendChild(el("div", { class: "codebase-empty-state" }, [
      el("p", { class: "codebase-empty-title", text: `No pair of ${nouns[data.rollup] || "paths"} is reserved together ${count(data.min_support)} or more times.` }),
      note(`${plural(below, "pair")} fall${below === 1 ? "s" : ""} under the threshold.${data.rollup === "file" ? " Single files are rarely reserved together often enough to rank; directories pool them." : ""}`),
      actions.length ? el("div", { class: "codebase-actions" }, actions) : null,
      largeNote ? note(largeNote) : null,
    ]));
    return keyed(box, "pairs", { coupling, rollup: data.rollup, min: data.min_support });
  }
  const grouped = view.groupRelease && release.length > 0;
  const list = el("ul", { class: "codebase-pair-list" });
  if (grouped) list.appendChild(releaseRow(release));
  for (const pair of pairs) if (!(grouped && releasePair(pair.paths))) list.appendChild(pairRow(pair));
  box.appendChild(list);
  const parts = [`${plural(below, "pair")} below ${count(data.min_support)} shared reservations ${below === 1 ? "is" : "are"} hidden.`];
  if (largeNote) parts.push(largeNote);
  box.appendChild(note(parts.join(" "), "codebase-box-note"));
  return keyed(box, "pairs", { coupling, rollup: data.rollup, min: data.min_support, grouped: view.groupRelease });
}

function lockRow(row, byWait, maxTotal, recorded) {
  const wait = row.wait && typeof row.wait === "object" ? row.wait : null;
  const total = wait ? waitDuration(wait.total_ms) : "—";
  const denied = `${count(row.numerator)} of ${count(row.denominator)} requesters denied`;
  const line = recorded && wait ? `median wait ${waitDuration(wait.median_ms)} · ${denied}` : denied;
  const item = el("li", { class: "codebase-lock-row" }, [
    el("div", { class: "codebase-lock-top" }, [fileLink(row.path), el("span", { class: "codebase-lock-total", text: recorded ? total : pct(row.rate) })]),
    rateBar(byWait ? (Number(wait?.total_ms) || 0) / (maxTotal || 1) : row.rate),
    el("div", { class: "codebase-lock-line" }, [
      el("span", { class: "codebase-extra", text: line }),
      recorded && wait && Number(wait.waiting) > 0 ? el("span", { class: "codebase-tag", text: `${count(wait.waiting)} still waiting` }) : null,
    ]),
  ]);
  item.dataset.path = row.path;
  return item;
}

function locksNode(model) {
  const { data } = model;
  const lock = section(data, "lock_contention");
  const recorded = lockWaitsRecorded(data);
  const byWait = lockByWait(data);
  const rows = byWait ? rowsByWait(data) : rowsOf(data, "lock_contention");
  const head = el("div", { class: "codebase-box-head" }, [
    el("div", {}, [el("h4", { text: "Lock waits" }), el("span", { class: "codebase-extra", text: recorded ? "time tasks spent denied a lock on the file" : "requesters denied a lock on the file" })]),
  ]);
  if (rowsByWait(data).length) {
    const sorts = el("div", { class: "codebase-segs small", role: "group", "aria-label": "Sort locks" });
    for (const [key, label] of [["wait", "Total wait"], ["rate", "Denial rate"]]) {
      const on = (key === "wait") === byWait;
      const button = el("button", { class: `codebase-seg${on ? " on" : ""}`, type: "button", text: label, "aria-pressed": String(on) });
      button.dataset.value = key;
      button.addEventListener("click", () => {
        if (view.lockSort === key) return;
        view = { ...view, lockSort: key };
        paint();
      });
      sorts.appendChild(button);
    }
    head.appendChild(sorts);
  }
  const box = el("section", { class: "codebase-box codebase-locks", "aria-label": "Lock waits" }, [head]);
  if (lockIsQuiet(data)) {
    box.appendChild(el("div", { class: "codebase-empty-state good" }, [
      el("p", { class: "codebase-empty-title", text: "No lock was refused in this window." }),
      note(`Tasks took ${plural(lock.reservations, "file reservation")} and none was denied, so no path has contention to rank.`),
    ]));
  } else if (!rows.length) {
    const below = Number(lock.below_support) || 0;
    box.appendChild(el("p", { class: "codebase-empty", text: below ? `No path reaches minimum support ${count(data.min_support)}; ${count(below)} fall below it.` : "No path has lock data in this window." }));
  } else {
    const maxTotal = Math.max(0, ...rows.map((row) => Number(row.wait?.total_ms) || 0));
    const list = el("ul", { class: "codebase-lock-list" });
    for (const row of rows) list.appendChild(lockRow(row, byWait, maxTotal, recorded));
    box.appendChild(list);
  }
  const foot = [];
  if (Number(lock.below_support) > 0 && rows.length) foot.push(`${plural(lock.below_support, "path")} below minimum support ${count(data.min_support)} hidden.`);
  if (!recorded) foot.push("Lock wait time is not recorded, so waits cannot be shown.");
  else if (rowsByWait(data).length) foot.push("A wait runs from a task's first denial to its admission or its leaving the backlog; its retries add no wait. A wait still in progress is measured to now.");
  if (foot.length) box.appendChild(note(foot.join(" "), "codebase-box-note"));
  return keyed(box, "locks", { lock, min: data.min_support, byWait });
}

function couplingNodes(model) {
  return [pairsNode(model), locksNode(model)];
}

// ---- marks ------------------------------------------------------------------

// A rate with the counts behind it, so a small sample reads as one.
function sampleNode(rates) {
  const denominator = Number(rates && rates.denominator) || 0;
  if (!denominator) return el("td", { class: "codebase-risk-none", text: "no tasks" });
  return el("td", {}, [
    el("span", { class: "codebase-rate", text: pct(rates.rate) }),
    " ",
    el("span", { class: "codebase-frac", text: `${count(rates.numerator)}/${count(denominator)}` }),
  ]);
}

/// Regression-risk marks: the marked paths' regression rate for tasks
/// delivered with marks against the same paths before the first mark.
function marksNode(model) {
  const marks = section(model.data, "regressions").risk_marks || {};
  const box = el("section", { class: "codebase-box codebase-risk-marks", "aria-label": "Regression-risk marks" }, [
    el("div", { class: "codebase-box-head" }, [el("h4", { text: "Regression-risk marks: with marks vs before" })]),
  ]);
  const body = el("div", { class: "codebase-box-body" });
  box.appendChild(body);
  if (!marks.enabled_at) {
    body.appendChild(note("No implement or review prompt has marked a regression-prone file yet, so there is nothing to compare."));
    return keyed(box, "marks", { marks });
  }
  const header = (text, scope) => {
    const cell = el("th", { text });
    cell.setAttribute("scope", scope);
    return cell;
  };
  const rowNode = (label, row, total) => {
    const name = header(total ? label : "", "row");
    if (!total) name.appendChild(viewLink({ view: "file", path: label }, [pathNode(label)], "codebase-path-link"));
    const item = el("tr", { class: total ? "codebase-risk-total" : "" }, [name, sampleNode(row.with_marks), sampleNode(row.before)]);
    item.dataset.path = total ? "" : label;
    return item;
  };
  const tbody = el("tbody", {}, [
    rowNode(`All ${plural(marks.marked_paths, "marked path")}`, marks, true),
    ...(Array.isArray(marks.rows) ? marks.rows : []).map((row) => rowNode(row.path, row, false)),
  ]);
  const head = el("tr", {}, [header("", "col"), header("With marks", "col"), header(`${count(marks.window_days)} days before`, "col")]);
  body.appendChild(el("div", { class: "codebase-hot-scroll" }, [
    el("table", { class: "codebase-risk-table", "aria-label": "Regression rate on marked paths with marks and before" }, [el("thead", {}, [head]), tbody]),
  ]));
  body.appendChild(note(`With marks: done tasks in this window whose implement or review prompt marked the path, regressed ÷ marked. Before: done tasks that touched the path in the ${count(marks.window_days)} days before the first mark (${String(marks.enabled_at).slice(0, 10)}). Recent tasks have had less time to be named by a regression.`));
  return keyed(box, "marks", { marks, query: buildCodebaseHash({ view: "hotspots" }) });
}

function marksNodes(model) {
  return [keyed(el("div", { class: "codebase-file-head" }, [backLink()]), "marks-head", { back: buildCodebaseHash({ view: "hotspots" }) }), marksNode(model)];
}

// ---- render -----------------------------------------------------------------

function paint() {
  const body = $(BODY_ID);
  if (!body || !lastData) return;
  const model = buildModel(lastData);
  const nodes = route.view === "file" ? fileNodes(model)
    : route.view === "coupling" ? couplingNodes(model)
      : route.view === "marks" ? marksNodes(model)
        : hotspotNodes(model);
  body.dataset.view = route.view;
  syncNodes(body, nodes);
  // Opening another view starts at its top, not where the last one scrolled.
  const opened = `${route.view}\n${route.path ?? ""}`;
  if (paintedView !== null && paintedView !== opened) {
    const panel = $("codebase-panel");
    if (panel && typeof panel.scrollIntoView === "function") panel.scrollIntoView({ block: "start" });
  }
  paintedView = opened;
}

function renderCodebase(data) {
  if (!data) return;
  lastData = data;
  lastPayloadKey = JSON.stringify(data);
  const computed = data.computed_at ? formatDateTime(data.computed_at) : "—";
  setMeta(`${plural(data.tasks_scanned, "task")} scanned · computed ${computed}`);
  paint();
  ensureRelated();
}

export {
  applyCodebaseHashQuery,
  buildCodebaseHash,
  fetchAndRenderCodebase,
  getCodebaseQuery,
  getCodebaseRoute,
  renderCodebase,
  wireCodebaseControls,
};

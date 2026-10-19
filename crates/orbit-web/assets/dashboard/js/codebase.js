// Analytics › Codebase: which parts of the repository are expensive or risky for
// agents to change, ranked from records Orbit already keeps
// (/api/codebase/analytics). "Touched" is the declared footprint, not the
// landed diff; the Task pilot ranking alone grades selectors against landed
// diffs. The server caches one scan per workspace; the window, minimum
// support and rollup only slice it, so the 30 s poll never rescans bundles.
//
// The view is eight ranking tiles, the list of the selected tile and a detail
// panel for the selected row. Tile, row and the other view choices live here,
// not in the hash, and every node is keyed and hashed for `syncNodes`, so a
// poll that answers the same payload leaves the page exactly as it was.

import { captureWorkspaceVisit, onWorkspaceChange, el, fetchJson, isAggregateView, renderPanelPlaceholder, requestPanel, syncNodes, taskHref, formatDateTime } from './common.js';

const $ = (id) => document.getElementById(id);

const BODY_ID = "codebase-body";
const WINDOWS = ["7d", "30d", "90d", "all"];
const ROLLUPS = ["file", "dir", "crate"];
const DEFAULT_QUERY = { window: "90d", min_support: 15, rollup: "file" };
// The minimum-support stepper walks this ladder; a hashed value off it steps
// to its neighbours.
const SUPPORT_STEPS = [1, 2, 3, 5, 10, 15, 20, 30, 50, 100];
// "Show paths with 5+ tasks" and "Lower to 5".
const LOWERED_SUPPORT = 5;
// A task that regressed this many ranked paths is marked: one fix there lifts
// several rows.
const WIDE_TASK_PATHS = 5;
// Regressed tasks shown in the detail panel before "All N".
const TASKS_SHOWN = 3;

// The four path rankings the overlap list joins.
const PATH_RANKINGS = ["regressions", "footprint_misses", "blocked", "rejected"];
const RANKINGS = {
  regressions: {
    label: "Regressions", short: "Regressed", unit: "done", below: "too few done tasks",
    definition: "Done tasks that a later task named in regression_from ÷ done tasks that declared the path, for tasks done in the window.",
  },
  footprint_misses: {
    label: "Footprint misses", short: "Footprint", unit: "declared", below: "too few tasks",
    definition: "Tasks that widened context_files onto the path mid-run ÷ tasks that declared it. A high rate means agents keep discovering they need this path.",
  },
  blocked: {
    label: "Blocked by a run", short: "Blocked", unit: "declared", below: "too few tasks",
    definition: "Tasks a run failure blocked ÷ tasks that declared the path. Infrastructure causes (auth, sandbox, disk, provider limits) and manual blocks are counted by cause, never against paths; a base conflict counts only against the conflicting paths it recorded.",
  },
  rejected: {
    label: "Rejected after a run", short: "Rejected", unit: "with a run", below: "too few tasks with a run",
    definition: "Rejected tasks that had a job run ÷ tasks with a run that declared the path.",
  },
  change_coupling: {
    label: "Change coupling", below: "too few shared",
    definition: "How often two paths are reserved together: reservations holding both ÷ reservations holding either (Jaccard). Reservations naming many files are left out.",
  },
  lock_contention: {
    label: "Lock contention", below: "too few requesters",
    definition: "Requesters denied a lock on the path ÷ requesters that reserved or were denied it. Retries by one task or run count once.",
  },
  task_pilot: {
    label: "Task pilot", below: "below support",
    definition: "Each task-pilot crew and model, graded on the done tasks it assessed against the files their landed commit changed. Recall: landed files a pilot selector covers ÷ landed files. Precision: pilot selectors that cover a landed file ÷ pilot selectors; an unused selector still holds a context lock other tasks wait on. F1 is their harmonic mean. Lift is the F1 gain over the filer's own selectors from before any pilot ran. A dir: or wildcard selector covers the files beneath or matching it, and a mirrored or generated file (plugin/skills, CLI and MCP goldens, docs/INDEX.md) counts as covered when its source is. Bulk tasks (more than 12 landed files) and no-diff-expected tasks are left out. Ranked by F1, for crews with at least the minimum assessed tasks.",
  },
};
const OVERLAP = {
  label: "In several rankings",
  title: "Paths in more than one ranking",
};
const TILE_ORDER = ["overlap", ...PATH_RANKINGS, "change_coupling", "lock_contention", "task_pilot"];
// Tasks listed under a pilot row's misses and over-declarations before "All".
const PILOT_TASKS_SHOWN = 3;
const ROLLUP_NOUNS = { file: "File", dir: "Directory", crate: "Crate" };

let codebaseQuery = { ...DEFAULT_QUERY };
// What the reader picked on the page: the tile, the row (`path:<path>` or
// `pair:<left>\n<right>` or `pilot:<crew>\n<model>`), whether expected pairs
// are hidden, whether lock contention is sorted by total wait, whether every
// regressed task is listed (`true`) or which pilot task list is opened in
// full (its label), and which path was last copied.
let view = { tile: "overlap", selected: null, hideExpected: false, lockByWait: false, allTasks: false, copied: null };
let lastData = null;
let relatedPath = "";
let relatedData = null;
let relatedRequest = 0;
let relatedSort = "jaccard";
let requestHashChange = () => {};
onWorkspaceChange(() => {
  lastData = null;
  relatedPath = "";
  relatedData = null;
  relatedRequest += 1;
  for (const id of ["codebase-path-options", "codebase-related-results"]) { if ($(id)) $(id).textContent = ""; }
  if ($("codebase-path-search")) $("codebase-path-search").value = "";
});

// An invalid value falls back to `fallback`: the default for a hash, the
// current selection for a control edit.
function normalizedQuery(raw, fallback = DEFAULT_QUERY) {
  const window = WINDOWS.includes(raw.window) ? raw.window : fallback.window;
  const support = Number(raw.min_support);
  const min_support = Number.isSafeInteger(support) && support >= 1 ? support : fallback.min_support;
  const rollup = ROLLUPS.includes(raw.rollup) ? raw.rollup : fallback.rollup;
  return { window, min_support, rollup };
}

/// Read the view's selection from an `#analytics/codebase?…` hash query.
function applyCodebaseHashQuery(query) {
  codebaseQuery = normalizedQuery({
    window: query.get("window"),
    min_support: query.get("min_support") ?? DEFAULT_QUERY.min_support,
    rollup: query.get("rollup"),
  });
  syncCodebaseControls();
}

/// The view's hash, naming the non-default parts of the selection, so a
/// reload or a shared link restores the same view.
function buildCodebaseHash() {
  const sp = new URLSearchParams();
  if (codebaseQuery.window !== DEFAULT_QUERY.window) sp.set("window", codebaseQuery.window);
  if (codebaseQuery.min_support !== DEFAULT_QUERY.min_support) sp.set("min_support", String(codebaseQuery.min_support));
  if (codebaseQuery.rollup !== DEFAULT_QUERY.rollup) sp.set("rollup", codebaseQuery.rollup);
  const qs = sp.toString();
  return qs ? `#analytics/codebase?${qs}` : "#analytics/codebase";
}

function getCodebaseQuery() {
  return { ...codebaseQuery };
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
    support.value = String(codebaseQuery.min_support);
    support.textContent = String(codebaseQuery.min_support);
  }
  const down = $("codebase-min-down");
  if (down) down.disabled = codebaseQuery.min_support <= 1;
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

function steppedSupport(current, direction) {
  if (direction < 0) return [...SUPPORT_STEPS].reverse().find((step) => step < current) ?? 1;
  return SUPPORT_STEPS.find((step) => step > current) ?? current + SUPPORT_STEPS.at(-1);
}

/// Bind the window and group-by segments and the minimum-support stepper.
/// `onChange` runs after the selection changes, to rewrite the hash.
function wireCodebaseControls(onChange) {
  requestHashChange = onChange;
  for (const [id, field] of [["codebase-window", "window"], ["codebase-rollup", "rollup"]]) {
    for (const button of segmentButtons(id)) {
      button.addEventListener("click", () => updateQuery({ [field]: button.dataset.value }));
    }
  }
  const step = (id, direction) => {
    const button = $(id);
    if (button) button.addEventListener("click", () => updateQuery({ min_support: steppedSupport(codebaseQuery.min_support, direction) }));
  };
  step("codebase-min-down", -1);
  step("codebase-min-up", 1);
  const search = $("codebase-path-search");
  if (search) search.addEventListener("input", renderPathOptions);
  const form = $("codebase-related-form");
  if (form) form.addEventListener("submit", (event) => {
    event.preventDefault();
    fetchRelated(search.value);
  });
  syncCodebaseControls();
}

function analyticsPath() {
  const sp = new URLSearchParams();
  sp.set("window", codebaseQuery.window);
  sp.set("min_support", String(codebaseQuery.min_support));
  sp.set("rollup", codebaseQuery.rollup);
  return `/api/codebase/analytics?${sp.toString()}`;
}

function setMeta(text) {
  const meta = $("codebase-meta");
  if (meta) meta.textContent = text;
}

function fetchAndRenderCodebase() {
  if (isAggregateView()) {
    lastData = null;
    relatedPath = "";
    relatedData = null;
    relatedRequest += 1;
    for (const id of ["codebase-path-options", "codebase-related-results"]) { if ($(id)) $(id).textContent = ""; }
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

const pathKey = (path) => `path:${path}`;
const pairKey = (paths) => `pair:${(paths || []).join("\n")}`;
const pilotKey = (row) => `pilot:${row.crew ?? ""}\n${row.model ?? ""}`;
const crewName = (row) => row.crew || "crew not recorded";
const modelName = (row) => row.model || "model not recorded";

// A lift in percentage points, signed.
function points(delta) {
  const value = Math.round((Number(delta) || 0) * 100);
  return `${value > 0 ? "+" : value < 0 ? "−" : "±"}${Math.abs(value)} pts`;
}

// ---- model ------------------------------------------------------------------

/// Everything the tiles, list and detail read, derived from one payload and
/// the view state.
function buildModel(data) {
  // path -> { ranking: { rank, row } } over the four path rankings.
  const index = new Map();
  for (const key of PATH_RANKINGS) {
    rowsOf(data, key).forEach((row, at) => {
      if (!index.has(row.path)) index.set(row.path, {});
      index.get(row.path)[key] = { rank: at + 1, row };
    });
  }
  // Paths in two or more rankings, ordered by the server (the same list
  // `orbit codebase analytics` prints): by how many, then by the highest rate
  // outside Rejected (whose one-off rejections would otherwise lead).
  const overlaps = rowsOf(data, "overlap").map((row) => ({ path: row.path, keys: Array.isArray(row.rankings) ? row.rankings : [] }));
  // How many ranked regression paths each task regressed.
  const regressedPaths = new Map();
  for (const row of rowsOf(data, "regressions")) {
    for (const id of new Set((row.regressed_tasks || []).map((task) => task.task_id))) {
      regressedPaths.set(id, (regressedPaths.get(id) || 0) + 1);
    }
  }
  const pairs = rowsOf(data, "change_coupling").map((row, at) => ({ row, rank: at + 1, expected: expectedPair(row.paths) }));
  const limit = Number(data.row_limit) || Math.max(0, ...TILE_ORDER.slice(1).map((key) => rowsOf(data, key).length));
  const model = { data, index, overlaps, regressedPaths, pairs, limit };
  model.entries = listEntries(model);
  const keys = model.entries.map((entry) => entry.key);
  model.selected = keys.includes(view.selected) ? view.selected : (keys[0] ?? null);
  return model;
}

/// The selectable rows of the open tile, in order.
function listEntries(model) {
  const { data, overlaps, pairs } = model;
  if (view.tile === "overlap") return overlaps.map((entry) => ({ key: pathKey(entry.path), path: entry.path }));
  if (view.tile === "change_coupling") {
    return pairs.filter((pair) => !(view.hideExpected && pair.expected)).map((pair) => ({ key: pairKey(pair.row.paths), pair }));
  }
  if (view.tile === "task_pilot") return rowsOf(data, "task_pilot").map((row, at) => ({ key: pilotKey(row), pilot: { row, rank: at + 1 } }));
  const rows = view.tile === "lock_contention" ? lockRowsShown(data) : rowsOf(data, view.tile);
  return rows.map((row) => ({ key: pathKey(row.path), path: row.path }));
}

function causeTotals(data) {
  const blocked = section(data, "blocked");
  const causes = Array.isArray(blocked.causes) ? blocked.causes : [];
  const byKind = (kind) => causes.filter((cause) => cause.kind === kind).reduce((sum, cause) => sum + (Number(cause.tasks) || 0), 0);
  const total = causes.reduce((sum, cause) => sum + (Number(cause.tasks) || 0), 0);
  const events = Array.isArray(blocked.unclassified_events) ? blocked.unclassified_events : [];
  return {
    causes, total, events, attributed: byKind("attributed"), unclassified: byKind("unclassified"),
    notFailures: Number(blocked.not_failures) || 0,
  };
}

function maxRejected(data) {
  return rowsOf(data, "rejected").reduce((max, row) => Math.max(max, Number(row.numerator) || 0), 0);
}

// Whether the payload carries lock waits. An older one does not, and the
// panel renders it as it always did.
function lockWaitsRecorded(data) {
  return section(data, "lock_contention").wait_time_recorded === true;
}

// The supported rows ranked by total wait; empty when the payload records no
// waits or no task waited, which also hides the sort.
function rowsByWait(data) {
  const rows = section(data, "lock_contention").rows_by_wait;
  return lockWaitsRecorded(data) && Array.isArray(rows) ? rows : [];
}

function lockRowsShown(data) {
  const byWait = rowsByWait(data);
  return view.lockByWait && byWait.length ? byWait : rowsOf(data, "lock_contention");
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

function rateBar(rate) {
  const width = Math.max(2, Math.min(100, Math.round((Number(rate) || 0) * 100)));
  return el("span", { class: "codebase-bar", "aria-hidden": "true" }, [el("span", { style: { width: `${width}%` } })]);
}

function metricNode(rate, numerator, denominator) {
  return el("span", { class: "codebase-metric" }, [
    rateBar(rate),
    el("span", { class: "codebase-rate", text: pct(rate) }),
    el("span", { class: "codebase-frac", text: `${count(numerator)} of ${count(denominator)}` }),
  ]);
}

function chip(ranking, text, fraction) {
  const node = el("span", { class: "codebase-chip" }, [
    el("span", { class: "codebase-dot", "aria-hidden": "true" }),
    `${RANKINGS[ranking].short} `,
    el("span", { class: "codebase-chip-rate", text }),
  ]);
  node.dataset.ranking = ranking;
  if (fraction) node.appendChild(el("span", { class: "codebase-chip-frac", text: fraction }));
  return node;
}

function note(text, extraClass = "") {
  return el("p", { class: `codebase-note${extraClass ? ` ${extraClass}` : ""}`, text });
}

function hint(text, tone = "") {
  const node = el("div", { class: `codebase-hint${tone ? ` ${tone}` : ""}`, role: tone === "warn" ? "note" : null }, [el("span", { text })]);
  return node;
}

function actionButton(label, onClick, extraClass = "") {
  const button = el("button", { class: `codebase-btn${extraClass ? ` ${extraClass}` : ""}`, type: "button", text: label });
  button.addEventListener("click", onClick);
  return button;
}

function withId(node, id) {
  node.id = id;
  return node;
}

function keyed(node, key, hash) {
  node.dataset.key = key;
  node.dataset.hash = JSON.stringify(hash);
  return node;
}

function select(key) {
  if (view.selected === key) return;
  view = { ...view, selected: key, allTasks: false, copied: null };
  paint();
  // Stacked on a phone, the detail sits below the list; bring it into view.
  const detail = $("codebase-detail");
  const list = $("codebase-list");
  if (detail && list && typeof detail.scrollIntoView === "function"
    && detail.getBoundingClientRect().top >= list.getBoundingClientRect().bottom) {
    detail.scrollIntoView({ block: "nearest" });
  }
}

// ---- tiles ------------------------------------------------------------------

function tileModels(model) {
  const { data, index, overlaps, pairs } = model;
  const min = data.min_support;
  const belowFoot = (key) => `${rowsOf(data, key).length} ranked · ${count(section(data, key).below_support)} ${RANKINGS[key].below}`;
  const rateTile = (key) => {
    const top = rowsOf(data, key)[0];
    return {
      key, label: RANKINGS[key].label,
      big: top ? pct(top.rate) : "None",
      sub: top ? `top: ${splitPath(top.path).base}` : "nothing ranked",
      foot: belowFoot(key),
    };
  };
  const three = overlaps.filter((entry) => entry.keys.length >= 3).length;
  const overlap = {
    key: "overlap", label: OVERLAP.label,
    big: overlaps.length ? plural(overlaps.length, "path") : "None",
    sub: `${count(three)} in three or more`,
    foot: `of ${plural(index.size, "ranked path")}`,
  };
  const { unclassified } = causeTotals(data);
  const blocked = { ...rateTile("blocked"), warn: unclassified > 0 ? `${plural(unclassified, "block")} unclassified` : null };
  const rejected = section(data, "rejected");
  const rejectedTile = {
    key: "rejected", label: RANKINGS.rejected.label,
    big: plural(rejected.rejected_with_run, "task"),
    sub: `at most ${count(maxRejected(data))} on any path`,
    foot: belowFoot("rejected"),
  };
  const topPair = pairs[0];
  const coupling = topPair
    ? {
      key: "change_coupling", label: RANKINGS.change_coupling.label, big: pct(topPair.row.rate),
      sub: `top: ${(topPair.row.paths || []).map((path) => splitPath(path).base).join(" ↔ ")}`,
      foot: belowFoot("change_coupling"),
    }
    : { key: "change_coupling", label: RANKINGS.change_coupling.label, big: "None", sub: `no pair reaches ${count(min)}`, foot: belowFoot("change_coupling") };
  const lock = lockIsQuiet(data)
    ? { key: "lock_contention", label: RANKINGS.lock_contention.label, big: "None", sub: "no lock refused", foot: "nothing to rank", tone: "good" }
    : rateTile("lock_contention");
  const pilots = section(data, "task_pilot");
  const topPilot = rowsOf(data, "task_pilot")[0];
  const pilot = {
    key: "task_pilot", label: RANKINGS.task_pilot.label,
    big: topPilot ? `F1 ${pct(topPilot.f1)}` : "None",
    sub: topPilot ? `top: ${crewName(topPilot)} · ${modelName(topPilot)}` : `${plural(pilots.assessed, "assessed task")} in this window`,
    foot: `${rowsOf(data, "task_pilot").length} ranked · ${count(pilots.below_support)} ${RANKINGS.task_pilot.below}`,
  };
  return [overlap, rateTile("regressions"), rateTile("footprint_misses"), blocked, rejectedTile, coupling, lock, pilot];
}

function tilesNode(model) {
  const tiles = tileModels(model);
  const node = el("section", { class: "codebase-tiles", "aria-label": "Rankings" });
  for (const tile of tiles) {
    const on = view.tile === tile.key;
    const button = el("button", { class: `codebase-tile${on ? " on" : ""}`, type: "button", "aria-pressed": String(on) }, [
      el("span", { class: "codebase-tile-label" }, [el("span", { class: "codebase-dot", "aria-hidden": "true" }), tile.label]),
      el("span", { class: "codebase-tile-big", text: tile.big }),
      el("span", { class: "codebase-tile-sub", text: tile.sub, title: tile.sub }),
      el("span", { class: `codebase-tile-foot${tile.tone ? ` ${tile.tone}` : ""}`, text: tile.foot }),
    ]);
    if (tile.warn) button.appendChild(el("span", { class: "codebase-tile-foot warn", text: tile.warn }));
    button.dataset.tile = tile.key;
    button.addEventListener("click", () => {
      if (view.tile === tile.key) return;
      view = { ...view, tile: tile.key };
      paint();
    });
    node.appendChild(button);
  }
  return keyed(node, "tiles", { tiles, open: view.tile });
}

// ---- list -------------------------------------------------------------------

function rowButton(key, selected, children) {
  const button = el("button", { class: "codebase-row", type: "button", "aria-current": key === selected ? "true" : null }, children);
  button.addEventListener("click", () => select(key));
  return el("li", {}, [button]);
}

function rowList(items) {
  const list = el("ol", { class: "codebase-rows" });
  for (const item of items) list.appendChild(item);
  return list;
}

function listFoot(children) {
  return el("div", { class: "codebase-list-foot" }, children);
}

function overlapContent(model) {
  const { data, index, overlaps, selected } = model;
  const content = [];
  if (data.rollup === "crate") {
    content.push(hint("Almost every crate clears the threshold at this level, so most crates show up in every ranking and this list says little. Group by file or directory to find hotspots."));
  }
  if (overlaps.length === 0) {
    content.push(el("p", { class: "codebase-empty", text: "No path turns up in more than one ranking." }));
  } else {
    content.push(rowList(overlaps.map((entry) => {
      const hits = index.get(entry.path);
      return rowButton(pathKey(entry.path), selected, [
        el("span", { class: "codebase-rank", text: `${entry.keys.length}×` }),
        el("span", { class: "codebase-pathcell" }, [
          pathNode(entry.path),
          el("span", { class: "codebase-chips" }, entry.keys.map((key) =>
            chip(key, pct(hits[key].row.rate), `${count(hits[key].row.numerator)}/${count(hits[key].row.denominator)}`))),
        ]),
      ]);
    })));
  }
  content.push(listFoot([note(`${count(overlaps.length)} of the ${plural(index.size, "ranked path")} show up in two or more rankings. Change coupling and lock contention rank pairs and locks, so they are not counted here.`)]));
  return content;
}

function lowerSupportButton(data, label) {
  if (!(Number(data.min_support) > LOWERED_SUPPORT)) return null;
  return actionButton(label, () => updateQuery({ min_support: LOWERED_SUPPORT }));
}

function emptyRanking(data, key) {
  const below = Number(section(data, key).below_support) || 0;
  const text = below > 0
    ? `No path reaches minimum support ${count(data.min_support)}; ${count(below)} fall below it.`
    : "No path has data in this window.";
  return el("p", { class: "codebase-empty", text });
}

function rankingContent(model, key) {
  const { data, index, selected } = model;
  const rows = rowsOf(data, key);
  const content = [];
  if (key === "blocked") content.push(...causeNodes(data));
  if (key === "rejected") {
    const rejected = section(data, "rejected");
    const max = maxRejected(data);
    content.push(hint(`${plural(rejected.rejected_with_run, "task was", "tasks were")} rejected after a run in this window; ${count(rejected.excluded_without_run)} rejected at proposal are left out. The most on any one path is ${count(max)}${max <= 2 ? ", so treat this list as weak evidence." : "."}`));
  }
  if (rows.length === 0) content.push(emptyRanking(data, key));
  else {
    content.push(rowList(rows.map((row, at) => {
      const hits = index.get(row.path) || {};
      const meta = PATH_RANKINGS.filter((other) => other !== key && hits[other]).map((other) => chip(other, pct(hits[other].row.rate)));
      if (key === "footprint_misses" && Number(row.widenings) > Number(row.numerator)) {
        meta.push(el("span", { class: "codebase-extra", text: `${plural(row.widenings, "widening")}` }));
      }
      if (key === "blocked" && Number(row.unclassified) > 0) {
        meta.push(el("span", { class: "codebase-extra", text: `${count(row.unclassified)} more blocked, cause unknown` }));
      }
      const cell = [pathNode(row.path)];
      if (meta.length) cell.push(el("span", { class: "codebase-chips" }, meta));
      return rowButton(pathKey(row.path), selected, [
        el("span", { class: "codebase-rank", text: String(at + 1) }),
        el("span", { class: "codebase-pathcell" }, cell),
        metricNode(row.rate, row.numerator, row.denominator),
      ]);
    })));
  }
  const below = Number(section(data, key).below_support) || 0;
  content.push(listFoot([
    note(`${plural(below, "path")} below minimum support ${count(data.min_support)} ${below === 1 ? "is" : "are"} hidden.`),
    lowerSupportButton(data, `Show paths with ${LOWERED_SUPPORT}+ tasks`),
  ]));
  return content;
}

const CAUSE_LABELS = { run_failure: "Run failure", unclassified: "Unclassified", base_conflict: "Base conflict" };
const CAUSE_KINDS = {
  attributed: "counted against paths", base_conflict: "counted on its conflicting paths",
  unclassified: "no cause found", infrastructure: "infrastructure", manual: "set by hand",
};
// The unclassified events a hint names.
const HINT_EVENTS = 3;

// A cause row's label; a manual block names the kind of actor that made it.
function causeLabel(cause) {
  const raw = cause && typeof cause === "object" ? cause.cause : cause;
  const name = CAUSE_LABELS[raw] || String(raw || "").replace(/_/g, " ");
  const label = name.charAt(0).toUpperCase() + name.slice(1);
  return cause && cause.actor ? `${label} (${cause.actor})` : label;
}

function unclassifiedHint(data) {
  const { causes, total, attributed, unclassified, events } = causeTotals(data);
  if (unclassified <= 0) return null;
  const largest = Math.max(0, ...causes.map((cause) => Number(cause.tasks) || 0));
  const top = events.slice(0, HINT_EVENTS).map((entry) => `${entry.event} (${count(entry.tasks)})`);
  const named = top.length ? ` Most entered blocked through ${top.join(", ")}.` : "";
  const text = `${count(unclassified)} of ${count(total)} blocked tasks (${pct(unclassified / total)}) carry no recorded cause, so they count against no path.${named}`;
  return unclassified === largest
    ? hint(`${text} The rates below only reflect the ${count(attributed)} that a run failure explains.`, "warn")
    : hint(text);
}

function causeNodes(data) {
  const { causes, total, notFailures } = causeTotals(data);
  const order = { attributed: 0, base_conflict: 1, unclassified: 2, infrastructure: 3, manual: 4 };
  const ordered = [...causes].sort((a, b) => (order[a.kind] ?? 3) - (order[b.kind] ?? 3) || Number(b.tasks) - Number(a.tasks));
  const shown = ordered.filter((cause) => Number(cause.tasks) > 0);
  const nodes = [];
  if (total > 0) {
    const bar = el("div", {
      class: "codebase-causebar",
      role: "img",
      "aria-label": `${shown.map((cause) => `${causeLabel(cause)} ${cause.tasks}`).join(", ")} of ${total} blocked tasks`,
    });
    for (const cause of shown) {
      const segment = el("span", { title: `${causeLabel(cause)} ${cause.tasks}`, style: { flex: `${Number(cause.tasks)} 1 0` } });
      segment.dataset.kind = cause.kind;
      bar.appendChild(segment);
    }
    nodes.push(bar);
  }
  const legend = el("ul", { class: "codebase-legend" });
  for (const cause of ordered) {
    const item = el("li", { class: "codebase-cause" }, [
      el("span", { class: "codebase-dot", "aria-hidden": "true" }),
      `${causeLabel(cause)} `,
      el("span", { class: "codebase-cause-count", text: count(cause.tasks) }),
      el("span", { class: "codebase-cause-kind", text: CAUSE_KINDS[cause.kind] || cause.kind }),
    ]);
    item.dataset.cause = cause.cause;
    item.dataset.kind = cause.kind;
    if (cause.actor) item.dataset.actor = cause.actor;
    legend.appendChild(item);
  }
  nodes.push(legend);
  const unknown = unclassifiedHint(data);
  if (unknown) nodes.push(unknown);
  if (notFailures > 0) {
    nodes.push(note(`${plural(notFailures, "task was", "tasks were")} held for review evidence or cancelled by an operator; those blocks are not failures and are left out.`, "codebase-not-failure"));
  }
  return nodes;
}

function couplingContent(model) {
  const { data, pairs, selected } = model;
  const coupling = section(data, "change_coupling");
  const below = Number(coupling.below_support) || 0;
  const large = Number(coupling.excluded_large) || 0;
  const largeNote = large > 0 ? `${plural(large, "reservation")} naming more than ${count(coupling.large_footprint_cap)} files ${large === 1 ? "is" : "are"} left out of every pair.` : "";
  if (pairs.length === 0) {
    const nouns = { file: "files", dir: "directories", crate: "crates" };
    const actions = [];
    if (data.rollup !== "dir") actions.push(actionButton("Group by directory", () => updateQuery({ rollup: "dir" })));
    const lower = lowerSupportButton(data, `Lower to ${LOWERED_SUPPORT}`);
    if (lower) actions.push(lower);
    return [el("div", { class: "codebase-empty-state" }, [
      el("p", { class: "codebase-empty-title", text: `No pair of ${nouns[data.rollup] || "paths"} is reserved together ${count(data.min_support)} or more times.` }),
      note(`${plural(below, "pair")} fall${below === 1 ? "s" : ""} under the threshold.${data.rollup === "file" ? " Single files are rarely reserved together often enough to rank; directories pool them." : ""}`),
      actions.length ? el("div", { class: "codebase-actions" }, actions) : null,
      largeNote ? note(largeNote) : null,
    ])];
  }
  const expected = pairs.filter((pair) => pair.expected).length;
  const visible = pairs.filter((pair) => !(view.hideExpected && pair.expected));
  const content = [];
  if (visible.length === 0) content.push(el("p", { class: "codebase-empty", text: "Every ranked pair is expected." }));
  else {
    content.push(rowList(visible.map((pair) => {
      const [left, right] = pair.row.paths || [];
      const cell = [pathNode(left), el("span", { class: "codebase-pair-second" }, [el("span", { class: "codebase-pair-arrow", text: "↔ " }), pathNode(right)])];
      if (pair.expected) cell.push(el("span", { class: "codebase-chips" }, [el("span", { class: "codebase-tag", text: "expected: its tests" })]));
      return rowButton(pairKey(pair.row.paths), selected, [
        el("span", { class: "codebase-rank", text: String(pair.rank) }),
        el("span", { class: "codebase-pathcell" }, cell),
        metricNode(pair.row.rate, pair.row.numerator, pair.row.denominator),
      ]);
    })));
  }
  const parts = [`${plural(below, "pair")} below ${count(data.min_support)} shared reservations ${below === 1 ? "is" : "are"} hidden.`];
  if (largeNote) parts.push(largeNote);
  if (expected) parts.push(`${count(expected)} of these pairs ${expected === 1 ? "is a directory" : "are directories"} with ${expected === 1 ? "its" : "their"} own tests/.`);
  content.push(listFoot([note(parts.join(" "))]));
  return content;
}

// A row's waits: median and longest, total minutes and any still waiting.
function waitChips(wait) {
  if (!wait || typeof wait !== "object") return [];
  const minutes = Math.round((Number(wait.total_ms) || 0) / 60000);
  const chips = [el("span", {
    class: "codebase-extra codebase-wait",
    text: `wait median ${waitDuration(wait.median_ms)} · max ${waitDuration(wait.max_ms)} · ${count(minutes)} min total`,
    title: `${plural(wait.episodes, "wait")} by tasks refused this lock`,
  })];
  if (Number(wait.waiting) > 0) chips.push(el("span", { class: "codebase-tag", text: `${count(wait.waiting)} still waiting` }));
  return chips;
}

function lockContent(model) {
  const { data, selected } = model;
  const lock = section(data, "lock_contention");
  const rows = lockRowsShown(data);
  const recorded = lockWaitsRecorded(data);
  const content = [];
  if (lockIsQuiet(data)) {
    content.push(el("div", { class: "codebase-empty-state good" }, [
      el("p", { class: "codebase-empty-title", text: "No lock was refused in this window." }),
      note(`Tasks took ${plural(lock.reservations, "file reservation")} and none was denied, so no path has contention to rank.`),
    ]));
  } else if (rows.length === 0) {
    content.push(emptyRanking(data, "lock_contention"));
  } else {
    content.push(rowList(rows.map((row, at) => {
      const waits = recorded ? waitChips(row.wait) : [];
      return rowButton(pathKey(row.path), selected, [
        el("span", { class: "codebase-rank", text: String(at + 1) }),
        el("span", { class: "codebase-pathcell" }, [
          pathNode(row.path),
          el("span", { class: "codebase-chips" }, [el("span", { class: "codebase-extra", text: `${plural(row.reservations, "reservation")} · ${plural(row.denials, "denial")}` })]),
          waits.length ? el("span", { class: "codebase-chips" }, waits) : null,
        ]),
        metricNode(row.rate, row.numerator, row.denominator),
      ]);
    })));
  }
  const foot = [];
  if (Number(lock.below_support) > 0) foot.push(`${plural(lock.below_support, "path")} below minimum support ${count(data.min_support)} hidden.`);
  if (!recorded) foot.push("Lock wait time is not recorded, so waits cannot be shown.");
  else if (rowsByWait(data).length) {
    foot.push("A wait runs from a task's first denial to its admission or its leaving the backlog; its retries add no wait. A wait still in progress is measured to now.");
  }
  if (foot.length) content.push(listFoot([note(foot.join(" "))]));
  return content;
}

function pilotContent(model) {
  const { data, selected } = model;
  const pilots = section(data, "task_pilot");
  const rows = rowsOf(data, "task_pilot");
  const below = Number(pilots.below_support) || 0;
  const content = [];
  if (rows.length === 0) {
    content.push(el("div", { class: "codebase-empty-state" }, [
      el("p", {
        class: "codebase-empty-title",
        text: below > 0
          ? `No pilot crew has ${count(data.min_support)} or more assessed tasks that landed in this window; ${plural(below, "crew")} fall${below === 1 ? "s" : ""} below it.`
          : "No task a pilot assessed landed in this window.",
      }),
      below > 0 ? el("div", { class: "codebase-actions" }, [lowerSupportButton(data, `Lower to ${LOWERED_SUPPORT}`)]) : null,
    ]));
  } else {
    content.push(rowList(rows.map((row, at) => rowButton(pilotKey(row), selected, [
      el("span", { class: "codebase-rank", text: String(at + 1) }),
      el("span", { class: "codebase-pathcell" }, [
        el("span", { class: "codebase-pilot" }, [
          el("span", { class: "codebase-pilot-crew", text: crewName(row) }),
          el("span", { class: "codebase-pilot-model", text: modelName(row) }),
        ]),
        el("span", { class: "codebase-chips" }, [
          el("span", { class: "codebase-extra", text: `recall ${pct(row.recall?.rate)} · precision ${pct(row.precision?.rate)}` }),
          el("span", { class: `codebase-tag ${Number(row.lift?.f1) >= 0 ? "lift" : "drop"}`, text: `${points(row.lift?.f1)} vs author`, title: `Filer's selectors: F1 ${pct(row.author?.f1)}` }),
          el("span", { class: "codebase-extra", text: `${(Number(row.widenings_per_task) || 0).toFixed(1)} widenings per task` }),
        ]),
      ]),
      el("span", { class: "codebase-metric" }, [
        rateBar(row.f1),
        el("span", { class: "codebase-rate", text: pct(row.f1) }),
        el("span", { class: "codebase-frac", text: plural(row.support, "task") }),
      ]),
    ]))));
  }
  const parts = [`${plural(pilots.assessed, "assessed task")} landed in this window.`];
  if (below > 0 && rows.length) parts.push(`${plural(below, "crew")} below minimum support ${count(data.min_support)} ${below === 1 ? "is" : "are"} hidden.`);
  if (Number(pilots.unattributed) > 0) parts.push(`${plural(pilots.unattributed, "assessment")} could not be matched to a pilot run.`);
  if (Number(pilots.without_assessment) > 0) parts.push(`${plural(pilots.without_assessment, "landed task")} had no pilot assessment.`);
  const left = (Number(pilots.excluded_large) || 0) + (Number(pilots.excluded_no_diff) || 0);
  if (left > 0) parts.push(`${count(pilots.excluded_large)} bulk and ${count(pilots.excluded_no_diff)} no-diff tasks are left out.`);
  content.push(listFoot([note(parts.join(" "))]));
  return content;
}

function listNode(model) {
  const { data, pairs } = model;
  const tile = view.tile;
  const head = el("div", { class: "codebase-list-head" });
  const titles = el("div", { class: "codebase-list-titles" }, [
    withId(el("h3", { class: "codebase-list-title" }, [
      el("span", { class: "codebase-dot", "aria-hidden": "true" }),
      tile === "overlap" ? OVERLAP.title : RANKINGS[tile].label,
    ]), "codebase-list-title"),
    el("p", {
      class: "codebase-definition",
      text: tile === "overlap"
        ? `Each ranking keeps its top ${count(model.limit)}. A path that turns up in several is a stronger lead than any single rate. Ordered by how many rankings, then by the highest rate outside Rejected.`
        : RANKINGS[tile].definition,
    }),
  ]);
  head.appendChild(titles);
  if (tile === "change_coupling" && pairs.some((pair) => pair.expected)) {
    const toggle = actionButton("Hide expected pairs", () => {
      view = { ...view, hideExpected: !view.hideExpected };
      paint();
    }, `small toggle${view.hideExpected ? " on" : ""}`);
    toggle.setAttribute("aria-pressed", String(view.hideExpected));
    head.appendChild(toggle);
  }
  if (tile === "lock_contention" && rowsByWait(data).length) {
    const toggle = actionButton("Sort by total wait", () => {
      view = { ...view, lockByWait: !view.lockByWait };
      paint();
    }, `small toggle${view.lockByWait ? " on" : ""}`);
    toggle.setAttribute("aria-pressed", String(view.lockByWait));
    head.appendChild(toggle);
  }
  const content = tile === "overlap" ? overlapContent(model)
    : tile === "change_coupling" ? couplingContent(model)
      : tile === "lock_contention" ? lockContent(model)
        : tile === "task_pilot" ? pilotContent(model)
          : rankingContent(model, tile);
  const node = withId(el("section", { class: "codebase-list", "aria-labelledby": "codebase-list-title" }, [head, ...content]), "codebase-list");
  node.dataset.ranking = tile;
  const hashed = tile === "overlap" ? [data.rollup, model.overlaps, PATH_RANKINGS.map((key) => rowsOf(data, key))]
    : tile === "blocked" ? section(data, "blocked")
      : section(data, tile);
  // Path lists also render chips from the other rankings. A refreshed scan
  // can change those memberships or rates without changing this section.
  const chipRates = PATH_RANKINGS.includes(tile) ? rowsOf(data, tile).map((row) =>
    PATH_RANKINGS.filter((key) => key !== tile).map((key) => {
      const hit = model.index.get(row.path)?.[key];
      return hit ? [key, hit.row.rate] : null;
    })) : null;
  return keyed(node, "list", { tile, selected: model.selected, hideExpected: view.hideExpected, lockByWait: view.lockByWait, min: data.min_support, rollup: data.rollup, limit: model.limit, hashed, chipRates });
}

// ---- detail -----------------------------------------------------------------

function regressedTaskNode(task, regressedPaths) {
  const wide = regressedPaths.get(task.task_id) || 0;
  const link = el("a", { class: "codebase-task-link", text: task.task_id });
  link.setAttribute("href", taskHref(task.task_id));
  const top = el("span", { class: "codebase-task-top" }, [link, el("span", { class: "codebase-crew", text: task.crew || "unassigned" })]);
  if (wide >= WIDE_TASK_PATHS) {
    top.appendChild(el("span", { class: "codebase-tag wide", text: `${count(wide)} ranked paths`, title: "This task regressed several ranked paths at once" }));
  }
  const by = el("span", { class: "codebase-task-by" }, ["regressed by"]);
  for (const id of task.regression_tasks || []) {
    const regression = el("a", { class: "codebase-task-link", text: id });
    regression.setAttribute("href", taskHref(id));
    by.append(" ");
    by.appendChild(regression);
  }
  const item = el("li", { class: "codebase-task" }, [top, el("span", { class: "codebase-task-title", text: task.title || "" }), by]);
  item.dataset.task = task.task_id;
  if (wide >= WIDE_TASK_PATHS) item.dataset.wide = String(wide);
  return item;
}

function regressionDetail(model, row) {
  const tasks = row.regressed_tasks || [];
  const nodes = [];
  const block = el("div", { class: "codebase-detail-block" });
  const head = el("div", { class: "codebase-detail-head" }, [el("p", { class: "codebase-eyebrow", text: "Regressed tasks" })]);
  if (tasks.length > TASKS_SHOWN) {
    const toggle = actionButton(view.allTasks === true ? "Show fewer" : `All ${count(tasks.length)} →`, () => {
      view = { ...view, allTasks: view.allTasks !== true };
      paint();
    }, "link");
    toggle.setAttribute("aria-expanded", String(view.allTasks === true));
    head.appendChild(toggle);
  }
  block.appendChild(head);
  const shown = view.allTasks === true ? tasks : tasks.slice(0, TASKS_SHOWN);
  const list = el("ul", { class: "codebase-tasks" });
  for (const task of shown) list.appendChild(regressedTaskNode(task, model.regressedPaths));
  block.appendChild(list);
  if (Number(row.numerator) > tasks.length) {
    block.appendChild(note(`The ${count(tasks.length)} most recently done of ${count(row.numerator)} regressed tasks are listed.`));
  }
  if (shown.some((task) => (model.regressedPaths.get(task.task_id) || 0) >= WIDE_TASK_PATHS)) {
    block.appendChild(note("A task marked with a path count regressed that many ranked paths at once, so a single fix there can lift several rows."));
  }
  nodes.push(block);
  const crews = el("div", { class: "codebase-detail-block" }, [el("p", { class: "codebase-eyebrow", text: "Regressions by crew" })]);
  const crewList = el("ul", { class: "codebase-crews" });
  for (const crew of row.by_crew || []) {
    const item = el("li", { class: "codebase-crew-row" }, [
      el("span", { class: "codebase-crew-name", text: crew.crew || "unassigned" }),
      rateBar(crew.rate),
      el("span", { class: "codebase-rate", text: pct(crew.rate) }),
      el("span", { class: "codebase-frac", text: `${count(crew.numerator)}/${count(crew.denominator)}` }),
    ]);
    item.dataset.crew = crew.crew || "unassigned";
    crewList.appendChild(item);
  }
  crews.appendChild(crewList);
  crews.appendChild(note("Crews take different complexity tiers, so their rates are not like for like."));
  nodes.push(crews);
  return nodes;
}

function copyPath(path) {
  const clipboard = globalThis.navigator && globalThis.navigator.clipboard;
  if (!clipboard || typeof clipboard.writeText !== "function") return;
  clipboard.writeText(path).then(() => {
    view = { ...view, copied: path };
    paint();
  }, () => {});
}

function pathDetail(model, path) {
  const { data, index, limit } = model;
  const hits = index.get(path) || {};
  const nodes = [el("div", { class: "codebase-detail-block" }, [
    withId(el("p", { class: "codebase-eyebrow", text: ROLLUP_NOUNS[data.rollup] || "Path" }), "codebase-detail-title"),
    pathNode(path, "large"),
    el("div", { class: "codebase-actions" }, [actionButton(view.copied === path ? "Copied" : "Copy path", () => copyPath(path), "small")]),
  ])];
  const rankings = el("ul", { class: "codebase-standings" });
  for (const key of PATH_RANKINGS) {
    const hit = hits[key];
    const item = el("li", { class: `codebase-standing${hit ? "" : " absent"}` }, [
      el("span", { class: "codebase-standing-label" }, [el("span", { class: "codebase-dot", "aria-hidden": "true" }), RANKINGS[key].label]),
      el("span", { class: "codebase-standing-rank", text: hit ? `#${hit.rank} of ${rowsOf(data, key).length}` : `not in the top ${count(limit)}` }),
    ]);
    item.dataset.ranking = key;
    if (hit) {
      item.appendChild(el("span", { class: "codebase-metric" }, [
        rateBar(hit.row.rate),
        el("span", { class: "codebase-rate", text: pct(hit.row.rate) }),
        el("span", { class: "codebase-frac", text: `${count(hit.row.numerator)} of ${count(hit.row.denominator)} ${RANKINGS[key].unit}` }),
      ]));
    }
    rankings.appendChild(item);
  }
  nodes.push(el("div", { class: "codebase-detail-block" }, [el("p", { class: "codebase-eyebrow", text: "Rankings" }), rankings]));
  if (hits.regressions) nodes.push(...regressionDetail(model, hits.regressions.row));
  else nodes.push(note("Not in the regression ranking, so no regressed tasks are listed."));
  return nodes;
}

function pairDetail(pair) {
  const [left, right] = pair.row.paths || [];
  const [leftCount, rightCount] = pair.row.reservations || [];
  return [
    el("div", { class: "codebase-detail-block" }, [
      withId(el("p", { class: "codebase-eyebrow", text: "Coupled pair" }), "codebase-detail-title"),
      pathNode(left, "large"),
      el("span", { class: "codebase-pair-second" }, [el("span", { class: "codebase-pair-arrow", text: "↔ " }), pathNode(right, "large")]),
      pair.expected ? el("span", { class: "codebase-chips" }, [el("span", { class: "codebase-tag", text: "expected: its tests" })]) : null,
    ]),
    el("div", { class: "codebase-detail-block" }, [
      el("span", { class: "codebase-metric" }, [rateBar(pair.row.rate), el("span", { class: "codebase-rate", text: pct(pair.row.rate) })]),
      el("p", { class: "codebase-pair-line", text: `Reserved together ${count(pair.row.numerator)} of ${count(pair.row.denominator)} reservations that held either.` }),
      note(`${splitPath(left).base} was reserved ${plural(leftCount, "time")}; ${splitPath(right).base} ${plural(rightCount, "time")}.`),
      note(pair.expected
        ? "Expected: a directory and its tests move together by design. Hide expected pairs to see the surprising ones."
        : "Tasks that change one keep needing the other, so a footprint that names one should probably name both."),
    ]),
  ];
}

function pilotTaskList(label, entries, items, empty) {
  const block = el("div", { class: "codebase-detail-block" });
  const head = el("div", { class: "codebase-detail-head" }, [el("p", { class: "codebase-eyebrow", text: label })]);
  const open = view.allTasks === label;
  if (entries.length > PILOT_TASKS_SHOWN) {
    const toggle = actionButton(open ? "Show fewer" : `All ${count(entries.length)} →`, () => {
      view = { ...view, allTasks: open ? false : label };
      paint();
    }, "link");
    toggle.setAttribute("aria-expanded", String(open));
    head.appendChild(toggle);
  }
  block.appendChild(head);
  if (entries.length === 0) {
    block.appendChild(note(empty));
    return block;
  }
  const list = el("ul", { class: "codebase-tasks" });
  for (const entry of open ? entries : entries.slice(0, PILOT_TASKS_SHOWN)) {
    const link = el("a", { class: "codebase-task-link", text: entry.task_id });
    link.setAttribute("href", taskHref(entry.task_id));
    const values = items(entry);
    const item = el("li", { class: "codebase-task" }, [
      el("span", { class: "codebase-task-top" }, [link, el("span", { class: "codebase-crew", text: plural(values.length, label === "Worst misses" ? "missed file" : "unused selector") })]),
      el("span", { class: "codebase-task-title", text: entry.title || "" }),
      el("span", { class: "codebase-pilot-paths" }, values.map((value) => label === "Worst misses" ? pathNode(value) : el("code", { class: "codebase-selector", text: value }))),
    ]);
    item.dataset.task = entry.task_id;
    list.appendChild(item);
  }
  block.appendChild(list);
  return block;
}

function pilotDetail(pilot, limit) {
  const { row, rank } = pilot;
  const standing = (label, pilotRate, authorRate, fraction) => {
    const item = el("li", { class: "codebase-standing" }, [
      el("span", { class: "codebase-standing-label" }, [el("span", { class: "codebase-dot", "aria-hidden": "true" }), label]),
      el("span", { class: "codebase-standing-rank", text: `author ${pct(authorRate)}` }),
      el("span", { class: "codebase-metric" }, [
        rateBar(pilotRate),
        el("span", { class: "codebase-rate", text: pct(pilotRate) }),
        fraction ? el("span", { class: "codebase-frac", text: fraction }) : null,
      ]),
    ]);
    item.dataset.metric = label.toLowerCase();
    return item;
  };
  const scores = el("ul", { class: "codebase-standings" }, [
    standing("F1", row.f1, row.author?.f1, null),
    standing("Recall", row.recall?.rate, row.author?.recall?.rate, `${count(row.recall?.numerator)} of ${count(row.recall?.denominator)} landed files`),
    standing("Precision", row.precision?.rate, row.author?.precision?.rate, `${count(row.precision?.numerator)} of ${count(row.precision?.denominator)} selectors`),
  ]);
  return [
    el("div", { class: "codebase-detail-block" }, [
      withId(el("p", { class: "codebase-eyebrow", text: `Task pilot #${rank}` }), "codebase-detail-title"),
      el("span", { class: "codebase-pilot large" }, [
        el("span", { class: "codebase-pilot-crew", text: crewName(row) }),
        el("span", { class: "codebase-pilot-model", text: modelName(row) }),
      ]),
      note(`${plural(row.support, "assessed task")} · ${points(row.lift?.f1)} F1 over the filer's selectors · ${plural(row.widenings, "mid-run widening")} (${(Number(row.widenings_per_task) || 0).toFixed(1)} per task)`),
    ]),
    el("div", { class: "codebase-detail-block" }, [el("p", { class: "codebase-eyebrow", text: "Scores" }), scores]),
    pilotTaskList("Worst misses", row.misses || [], (entry) => entry.paths || [], "Every landed file was covered by a pilot selector."),
    pilotTaskList("Largest over-declarations", row.over_declarations || [], (entry) => entry.selectors || [], "Every pilot selector covered a landed file."),
    note(`Tasks with the most landed files no pilot selector covered, and with the most selectors that covered no landed file; at most ${count(limit)} each.`),
  ];
}

function detailNode(model) {
  const key = model.selected;
  const entry = model.entries.find((candidate) => candidate.key === key);
  const content = !entry
    ? [note("Nothing is ranked here, so there is no row to show.")]
    : entry.pair ? pairDetail(entry.pair)
      : entry.pilot ? pilotDetail(entry.pilot, section(model.data, "task_pilot").drilldown_limit)
        : pathDetail(model, entry.path);
  const node = withId(el("aside", { class: "codebase-detail", "aria-labelledby": entry ? "codebase-detail-title" : null, "aria-label": entry ? null : "Detail" }, content), "codebase-detail");
  if (entry && entry.pair) node.dataset.ranking = "change_coupling";
  if (entry && entry.pilot) node.dataset.ranking = "task_pilot";
  const hits = entry && entry.path ? model.index.get(entry.path) : null;
  return keyed(node, "detail", {
    key, rollup: model.data.rollup, limit: model.limit, allTasks: view.allTasks, copied: view.copied === (entry && entry.path),
    hits, pair: entry && entry.pair ? entry.pair : null, pilot: entry && entry.pilot ? entry.pilot : null, lengths: PATH_RANKINGS.map((ranking) => rowsOf(model.data, ranking).length),
    wide: hits && hits.regressions ? (hits.regressions.row.regressed_tasks || []).map((task) => model.regressedPaths.get(task.task_id) || 0) : null,
  });
}

// ---- render -----------------------------------------------------------------

function paint() {
  const body = $(BODY_ID);
  if (!body || !lastData) return;
  const model = buildModel(lastData);
  syncNodes(body, [tilesNode(model), listNode(model), detailNode(model)]);
}

function renderCodebase(data) {
  if (!data) return;
  lastData = data;
  const computed = data.computed_at ? formatDateTime(data.computed_at) : "—";
  const minutes = Math.round(Number(data.cache_ttl_seconds) / 60) || 0;
  setMeta(`${plural(data.tasks_scanned, "task")} scanned · computed ${computed} · recomputed every ${minutes} min`);
  paint();
  renderPathOptions();
  if (relatedPath) fetchRelated(relatedPath);
}

function renderPathOptions() {
  const options = $("codebase-path-options");
  const search = $("codebase-path-search");
  if (!options || !search) return;
  options.textContent = "";
  if (isAggregateView() || !search.value) return;
  const paths = (lastData?.known_paths || []).filter(path => path.startsWith(search.value)).slice(0, 12);
  for (const path of paths) {
    const button = el("button", { class: "action", type: "button", text: path });
    button.addEventListener("click", () => {
      search.value = path;
      options.textContent = "";
      fetchRelated(path);
    });
    options.appendChild(button);
  }
}

async function fetchRelated(path) {
  const results = $("codebase-related-results");
  if (!results || isAggregateView()) return;
  relatedPath = path;
  const request = ++relatedRequest;
  const visit = captureWorkspaceVisit();
  const selection = JSON.stringify(codebaseQuery);
  const sp = new URLSearchParams({ path, window: codebaseQuery.window, min_support: String(codebaseQuery.min_support), top: "100" });
  results.textContent = "Loading related files…";
  try {
    const data = await fetchJson(`/api/codebase/related?${sp}`);
    if (request !== relatedRequest || !visit.isCurrent() || selection !== JSON.stringify(codebaseQuery)) return;
    relatedData = data;
    renderRelated();
  } catch (error) {
    if (request === relatedRequest && visit.isCurrent() && selection === JSON.stringify(codebaseQuery)) results.textContent = `Could not load related files: ${error.message}`;
  }
}

function renderRelated() {
  const results = $("codebase-related-results");
  if (!results || !relatedData) return;
  results.textContent = "";
  results.appendChild(el("h4", { text: relatedData.path }));
  results.appendChild(el("p", { class: "codebase-note", text: `${count(relatedData.path_support)} landed tasks changed this file · ${count(relatedData.excluded_large)} bulk tasks excluded · ${count(relatedData.unavailable_landed_tasks)} done tasks lack readable landed evidence. File-level measures use the selected window and minimum shared tasks.` }));
  const rows = [...(relatedData.rows || [])];
  if (!rows.length) {
    results.appendChild(el("p", { class: "codebase-empty", text: "No related files reach minimum shared support in this window." }));
    return;
  }
  const sorts = el("div", { class: "codebase-related-sorts", role: "group", "aria-label": "Sort related files" });
  for (const [key, label] of [["jaccard", "Landed Jaccard"], ["conditional_rate", "Conditional rate"], ["reservation_coupling", "Reservation coupling"], ["footprint_miss_count", "Footprint misses"]]) {
    const button = el("button", { class: "action", type: "button", text: label, "aria-pressed": String(relatedSort === key) });
    button.addEventListener("click", () => { relatedSort = key; renderRelated(); });
    sorts.appendChild(button);
  }
  results.appendChild(sorts);
  const metric = row => relatedSort === "reservation_coupling" ? (row.reservation_coupling?.rate ?? -1) : Number(row[relatedSort]);
  rows.sort((a, b) => metric(b) - metric(a) || b.support.both - a.support.both || a.path.localeCompare(b.path));
  const list = el("ul", { class: "codebase-related-list" });
  for (const row of rows) {
    const item = el("li", { class: "codebase-related-row" }, [pathNode(row.path), el("p", { text: `Landed ${pct(row.jaccard)} · conditional ${pct(row.conditional_rate)} · shared ${row.support.both} / either ${row.support.either} · file ${row.support.path}, partner ${row.support.partner} · reserved ${row.reservation_coupling ? `${pct(row.reservation_coupling.rate)} (${row.reservation_coupling.numerator}/${row.reservation_coupling.denominator})` : "—"} · ${count(row.footprint_miss_count)} footprint misses` })]);
    for (const [label, tasks] of [["Changed together", row.tasks], ["Reserved together", row.reservation_coupling?.tasks], ["Undeclared partner", row.footprint_miss_tasks]]) {
      if (!tasks?.length) continue;
      const evidence = el("details", {}, [el("summary", { text: `${label}: ${tasks.length} tasks` })]);
      for (const task of tasks) {
        const link = el("a", { class: "codebase-task-link", text: task.title, title: task.commit || "" });
        link.setAttribute("href", taskHref(task.task_id));
        evidence.appendChild(link);
      }
      item.appendChild(evidence);
    }
    list.appendChild(item);
  }
  results.appendChild(list);
}

export {
  applyCodebaseHashQuery,
  buildCodebaseHash,
  fetchAndRenderCodebase,
  getCodebaseQuery,
  renderCodebase,
  wireCodebaseControls,
};

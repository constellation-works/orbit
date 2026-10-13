// Analytics › Codebase: which parts of the repository are expensive or risky for
// agents to change, ranked from records Orbit already keeps
// (/api/codebase/analytics). "Touched" is the declared footprint, not the
// landed diff. The server caches one scan per workspace; the window, minimum
// support and rollup only slice it, so the 30 s poll never rescans bundles.
//
// The view is seven ranking tiles, the list of the selected tile and a detail
// panel for the selected row. Tile, row and the other view choices live here,
// not in the hash, and every node is keyed and hashed for `syncNodes`, so a
// poll that answers the same payload leaves the page exactly as it was.

import { el, fetchJson, isAggregateView, renderPanelPlaceholder, requestPanel, syncNodes, taskHref, formatDateTime } from './common.js';

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
    definition: "Tasks a run failure blocked ÷ tasks that declared the path. Infrastructure causes (auth, sandbox, disk, provider limits) are counted by cause, never against paths.",
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
};
const OVERLAP = {
  label: "In several rankings",
  title: "Paths in more than one ranking",
};
const TILE_ORDER = ["overlap", ...PATH_RANKINGS, "change_coupling", "lock_contention"];
const ROLLUP_NOUNS = { file: "File", dir: "Directory", crate: "Crate" };

let codebaseQuery = { ...DEFAULT_QUERY };
// What the reader picked on the page: the tile, the row (`path:<path>` or
// `pair:<left>\n<right>`), whether expected pairs are hidden, whether every
// regressed task is listed, and which path was last copied.
let view = { tile: "overlap", selected: null, hideExpected: false, allTasks: false, copied: null };
let lastData = null;
let requestHashChange = () => {};

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
  // Paths in two or more rankings, by how many, then by the highest rate
  // outside Rejected (whose one-off rejections would otherwise lead).
  const overlaps = [...index].map(([path, hits]) => {
    const keys = PATH_RANKINGS.filter((key) => hits[key]);
    const rates = keys.filter((key) => key !== "rejected").map((key) => Number(hits[key].row.rate) || 0);
    return { path, keys, top: rates.length ? Math.max(...rates) : 0 };
  })
    .filter((entry) => entry.keys.length >= 2)
    .sort((a, b) => b.keys.length - a.keys.length || b.top - a.top || (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
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
  return rowsOf(data, view.tile).map((row) => ({ key: pathKey(row.path), path: row.path }));
}

function causeTotals(data) {
  const causes = Array.isArray(section(data, "blocked").causes) ? section(data, "blocked").causes : [];
  const byKind = (kind) => causes.filter((cause) => cause.kind === kind).reduce((sum, cause) => sum + (Number(cause.tasks) || 0), 0);
  const total = causes.reduce((sum, cause) => sum + (Number(cause.tasks) || 0), 0);
  return { causes, total, attributed: byKind("attributed"), unclassified: byKind("unclassified") };
}

function maxRejected(data) {
  return rowsOf(data, "rejected").reduce((max, row) => Math.max(max, Number(row.numerator) || 0), 0);
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
  return [overlap, rateTile("regressions"), rateTile("footprint_misses"), blocked, rejectedTile, coupling, lock];
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

const CAUSE_LABELS = { run_failure: "Run failure", unclassified: "Unclassified" };
const CAUSE_KINDS = { attributed: "counted against paths", unclassified: "no cause found", infrastructure: "infrastructure" };

function causeLabel(cause) {
  const name = CAUSE_LABELS[cause] || String(cause || "").replace(/_/g, " ");
  return name.charAt(0).toUpperCase() + name.slice(1);
}

function causeNodes(data) {
  const { causes, total, attributed, unclassified } = causeTotals(data);
  const order = { attributed: 0, unclassified: 1, infrastructure: 2 };
  const ordered = [...causes].sort((a, b) => (order[a.kind] ?? 3) - (order[b.kind] ?? 3) || Number(b.tasks) - Number(a.tasks));
  const shown = ordered.filter((cause) => Number(cause.tasks) > 0);
  const nodes = [];
  if (total > 0) {
    const bar = el("div", {
      class: "codebase-causebar",
      role: "img",
      "aria-label": `${shown.map((cause) => `${causeLabel(cause.cause)} ${cause.tasks}`).join(", ")} of ${total} blocked tasks`,
    });
    for (const cause of shown) {
      const segment = el("span", { title: `${causeLabel(cause.cause)} ${cause.tasks}`, style: { flex: `${Number(cause.tasks)} 1 0` } });
      segment.dataset.kind = cause.kind;
      bar.appendChild(segment);
    }
    nodes.push(bar);
  }
  const legend = el("ul", { class: "codebase-legend" });
  for (const cause of ordered) {
    const item = el("li", { class: "codebase-cause" }, [
      el("span", { class: "codebase-dot", "aria-hidden": "true" }),
      `${causeLabel(cause.cause)} `,
      el("span", { class: "codebase-cause-count", text: count(cause.tasks) }),
      el("span", { class: "codebase-cause-kind", text: CAUSE_KINDS[cause.kind] || cause.kind }),
    ]);
    item.dataset.cause = cause.cause;
    item.dataset.kind = cause.kind;
    legend.appendChild(item);
  }
  nodes.push(legend);
  const largest = Math.max(0, ...causes.map((cause) => Number(cause.tasks) || 0));
  if (unclassified > 0 && unclassified === largest) {
    nodes.push(hint(`${count(unclassified)} of ${count(total)} blocked tasks (${pct(unclassified / total)}) carry no recognisable cause, so they count against no path. The rates below only reflect the ${count(attributed)} that a run failure explains.`, "warn"));
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

function lockContent(model) {
  const { data, selected } = model;
  const lock = section(data, "lock_contention");
  const rows = rowsOf(data, "lock_contention");
  const content = [];
  if (lockIsQuiet(data)) {
    content.push(el("div", { class: "codebase-empty-state good" }, [
      el("p", { class: "codebase-empty-title", text: "No lock was refused in this window." }),
      note(`Tasks took ${plural(lock.reservations, "file reservation")} and none was denied, so no path has contention to rank.`),
    ]));
  } else if (rows.length === 0) {
    content.push(emptyRanking(data, "lock_contention"));
  } else {
    content.push(rowList(rows.map((row, at) => rowButton(pathKey(row.path), selected, [
      el("span", { class: "codebase-rank", text: String(at + 1) }),
      el("span", { class: "codebase-pathcell" }, [
        pathNode(row.path),
        el("span", { class: "codebase-chips" }, [el("span", { class: "codebase-extra", text: `${plural(row.reservations, "reservation")} · ${plural(row.denials, "denial")}` })]),
      ]),
      metricNode(row.rate, row.numerator, row.denominator),
    ]))));
  }
  const foot = [];
  if (Number(lock.below_support) > 0) foot.push(`${plural(lock.below_support, "path")} below minimum support ${count(data.min_support)} hidden.`);
  if (lock.wait_time_recorded !== true) foot.push("Lock wait time is not recorded, so waits cannot be shown.");
  if (foot.length) content.push(listFoot([note(foot.join(" "))]));
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
  const content = tile === "overlap" ? overlapContent(model)
    : tile === "change_coupling" ? couplingContent(model)
      : tile === "lock_contention" ? lockContent(model)
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
  return keyed(node, "list", { tile, selected: model.selected, hideExpected: view.hideExpected, min: data.min_support, rollup: data.rollup, limit: model.limit, hashed, chipRates });
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
    const toggle = actionButton(view.allTasks ? "Show fewer" : `All ${count(tasks.length)} →`, () => {
      view = { ...view, allTasks: !view.allTasks };
      paint();
    }, "link");
    toggle.setAttribute("aria-expanded", String(view.allTasks));
    head.appendChild(toggle);
  }
  block.appendChild(head);
  const shown = view.allTasks ? tasks : tasks.slice(0, TASKS_SHOWN);
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

function detailNode(model) {
  const key = model.selected;
  const entry = model.entries.find((candidate) => candidate.key === key);
  const content = !entry
    ? [note("Nothing is ranked here, so there is no row to show.")]
    : entry.pair ? pairDetail(entry.pair) : pathDetail(model, entry.path);
  const node = withId(el("aside", { class: "codebase-detail", "aria-labelledby": entry ? "codebase-detail-title" : null, "aria-label": entry ? null : "Detail" }, content), "codebase-detail");
  if (entry && entry.pair) node.dataset.ranking = "change_coupling";
  const hits = entry && entry.path ? model.index.get(entry.path) : null;
  return keyed(node, "detail", {
    key, rollup: model.data.rollup, limit: model.limit, allTasks: view.allTasks, copied: view.copied === (entry && entry.path),
    hits, pair: entry && entry.pair ? entry.pair : null, lengths: PATH_RANKINGS.map((ranking) => rowsOf(model.data, ranking).length),
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
}

export {
  applyCodebaseHashQuery,
  buildCodebaseHash,
  fetchAndRenderCodebase,
  getCodebaseQuery,
  renderCodebase,
  wireCodebaseControls,
};

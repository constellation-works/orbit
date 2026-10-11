// Audit › Codebase: which parts of the repository are expensive or risky for
// agents to change, ranked from records Orbit already keeps
// (/api/codebase/analytics). "Touched" is the declared footprint, not the
// landed diff. The server caches one scan per workspace; the window, minimum
// support and rollup only slice it, so the 30 s poll never rescans bundles.

import { el, fetchJson, isAggregateView, renderPanelPlaceholder, requestPanel, syncNodes, taskHref, formatDateTime } from './common.js';

const $ = (id) => document.getElementById(id);

const BODY_ID = "audit-codebase-body";
const WINDOWS = ["7d", "30d", "90d", "all"];
const ROLLUPS = ["file", "dir", "crate"];
const DEFAULT_QUERY = { window: "90d", min_support: 15, rollup: "file" };

let codebaseQuery = { ...DEFAULT_QUERY };

// An invalid value falls back to `fallback`: the default for a hash, the
// current selection for a control edit.
function normalizedQuery(raw, fallback = DEFAULT_QUERY) {
  const window = WINDOWS.includes(raw.window) ? raw.window : fallback.window;
  const support = Number(raw.min_support);
  const min_support = Number.isSafeInteger(support) && support >= 1 ? support : fallback.min_support;
  const rollup = ROLLUPS.includes(raw.rollup) ? raw.rollup : fallback.rollup;
  return { window, min_support, rollup };
}

/// Read the view's selection from an `#audit/codebase?…` hash query.
function applyCodebaseHashQuery(query) {
  codebaseQuery = normalizedQuery({
    window: query.get("window"),
    min_support: query.get("min_support") ?? DEFAULT_QUERY.min_support,
    rollup: query.get("rollup"),
  });
  syncCodebaseControls();
}

/// Write the non-default parts of the selection into `sp`, so a reload or a
/// shared `#audit/codebase` link restores the same view.
function codebaseHashParams(sp) {
  if (codebaseQuery.window !== DEFAULT_QUERY.window) sp.set("window", codebaseQuery.window);
  if (codebaseQuery.min_support !== DEFAULT_QUERY.min_support) sp.set("min_support", String(codebaseQuery.min_support));
  if (codebaseQuery.rollup !== DEFAULT_QUERY.rollup) sp.set("rollup", codebaseQuery.rollup);
}

function getCodebaseQuery() {
  return { ...codebaseQuery };
}

function syncCodebaseControls() {
  const window = $("codebase-window");
  if (window) window.value = codebaseQuery.window;
  const support = $("codebase-min-support");
  if (support) support.value = String(codebaseQuery.min_support);
  const rollup = $("codebase-rollup");
  if (rollup) rollup.value = codebaseQuery.rollup;
}

/// Bind the window picker, minimum-support input and rollup select. `onChange`
/// runs after the selection changes, to rewrite the hash.
function wireCodebaseControls(onChange) {
  const bind = (id, event, update) => {
    const node = $(id);
    if (!node) return;
    node.addEventListener(event, () => {
      const next = normalizedQuery({ ...codebaseQuery, ...update(node.value) }, codebaseQuery);
      if (JSON.stringify(next) === JSON.stringify(codebaseQuery)) {
        syncCodebaseControls();
        return;
      }
      codebaseQuery = next;
      syncCodebaseControls();
      onChange();
    });
  };
  bind("codebase-window", "change", (value) => ({ window: value }));
  bind("codebase-min-support", "change", (value) => ({ min_support: value }));
  bind("codebase-rollup", "change", (value) => ({ rollup: value }));
  syncCodebaseControls();
}

function analyticsPath() {
  const sp = new URLSearchParams();
  sp.set("window", codebaseQuery.window);
  sp.set("min_support", String(codebaseQuery.min_support));
  sp.set("rollup", codebaseQuery.rollup);
  return `/api/codebase/analytics?${sp.toString()}`;
}

function fetchAndRenderCodebase() {
  if (isAggregateView()) {
    renderPanelPlaceholder(BODY_ID);
    return Promise.resolve();
  }
  const path = analyticsPath();
  return requestPanel(BODY_ID, path, () => fetchJson(path), (data) => renderCodebase(data));
}

function pct(rate) {
  const value = Number(rate);
  if (!Number.isFinite(value)) return "—";
  return `${(value * 100).toFixed(value > 0 && value < 0.1 ? 1 : 0)}%`;
}

function rateCells(row) {
  return [
    el("td", { class: "num codebase-rate", text: pct(row.rate) }),
    el("td", { class: "num codebase-ratio", text: `${row.numerator ?? 0} / ${row.denominator ?? 0}` }),
  ];
}

function taskLink(taskId) {
  const link = el("a", { class: "codebase-task-link", text: taskId });
  link.setAttribute("href", taskHref(taskId));
  return link;
}

function emptySection(section, data, noun = "path") {
  const below = Number(section && section.below_support) || 0;
  const text = below > 0
    ? `No ${noun} reaches minimum support ${data.min_support}; ${below} below it. Lower minimum support to see them.`
    : `No ${noun} has data in this window.`;
  return el("div", { class: "empty codebase-empty", text });
}

function belowSupportNote(section, data) {
  const below = Number(section && section.below_support) || 0;
  if (!below) return null;
  return el("div", { class: "codebase-note", text: `${below} below minimum support ${data.min_support} omitted.` });
}

function table(headers, rows) {
  const node = el("table", { class: "codebase-table" });
  const head = el("tr");
  for (const header of headers) {
    head.appendChild(el("th", { class: header.num ? "num" : "", text: header.label, title: header.title }));
  }
  node.appendChild(el("thead", {}, [head]));
  const body = el("tbody");
  for (const row of rows) body.appendChild(row);
  node.appendChild(body);
  return node;
}

function sectionNode(key, title, definition, section, content) {
  const node = el("section", { class: "codebase-section" });
  node.dataset.key = key;
  node.dataset.hash = JSON.stringify(section ?? null);
  node.dataset.section = key;
  node.appendChild(el("h5", { text: title }));
  node.appendChild(el("p", { class: "codebase-definition", text: definition }));
  for (const child of content) if (child) node.appendChild(child);
  return node;
}

function pathCell(path) {
  return el("td", { class: "codebase-path", text: path, title: path });
}

function regressionsSection(data) {
  const section = data.regressions || {};
  const rows = section.rows || [];
  const content = rows.length === 0 ? [emptySection(section, data)] : [table([
    { label: "path" },
    { label: "rate", num: true },
    { label: "regressed / done", num: true, title: "Done tasks with an incoming regression_from ÷ done tasks whose declared footprint touched the path" },
    { label: "drill-down" },
  ], rows.map((row) => {
    const tr = el("tr");
    tr.appendChild(pathCell(row.path));
    for (const cell of rateCells(row)) tr.appendChild(cell);
    const details = el("details", { class: "codebase-drilldown" });
    details.appendChild(el("summary", { text: `${(row.regressed_tasks || []).length} regressed` }));
    const list = el("ul", { class: "codebase-regressed" });
    for (const task of row.regressed_tasks || []) {
      const item = el("li");
      item.appendChild(taskLink(task.task_id));
      item.append(` ${task.title || ""}${task.crew ? ` · ${task.crew}` : ""} ← `);
      (task.regression_tasks || []).forEach((id, index) => {
        if (index) item.append(", ");
        item.appendChild(taskLink(id));
      });
      list.appendChild(item);
    }
    details.appendChild(list);
    const crews = (row.by_crew || []).map((crew) =>
      `${crew.crew || "unassigned"} ${pct(crew.rate)} (${crew.numerator}/${crew.denominator})`);
    if (crews.length) details.appendChild(el("div", { class: "codebase-by-crew", text: `By crew: ${crews.join(" · ")}` }));
    tr.appendChild(el("td", {}, [details]));
    return tr;
  })), belowSupportNote(section, data)];
  return sectionNode(
    "regressions",
    "Regression-prone",
    "Distinct done tasks with an incoming regression_from ÷ done tasks that touched the path, for tasks done in the window.",
    section,
    content,
  );
}

function lockSection(data) {
  const section = data.lock_contention || {};
  const rows = section.rows || [];
  const waitRecorded = section.wait_time_recorded === true;
  const content = rows.length === 0 ? [emptySection(section, data)] : [table([
    { label: "path" },
    { label: "rate", num: true },
    { label: "contending / requesting", num: true, title: "Distinct tasks denied a lock ÷ distinct tasks that reserved or were denied the path; a retry loop counts once" },
    { label: "reservations", num: true },
    { label: "denials", num: true },
    ...(waitRecorded ? [{ label: "wait", num: true }] : []),
  ], rows.map((row) => {
    const tr = el("tr");
    tr.appendChild(pathCell(row.path));
    for (const cell of rateCells(row)) tr.appendChild(cell);
    tr.appendChild(el("td", { class: "num", text: String(row.reservations ?? 0) }));
    tr.appendChild(el("td", { class: "num", text: String(row.denials ?? 0) }));
    if (waitRecorded) tr.appendChild(el("td", { class: "num", text: row.wait_ms == null ? "—" : `${row.wait_ms} ms` }));
    return tr;
  })), belowSupportNote(section, data)];
  if (!waitRecorded) content.push(el("div", { class: "codebase-note", text: "Lock wait time is not recorded, so it is not shown." }));
  return sectionNode(
    "lock_contention",
    "Lock contention",
    "Distinct contending tasks ÷ distinct tasks that requested the path. Reservations and denial attempts are counted separately.",
    section,
    content,
  );
}

function footprintSection(data) {
  const section = data.footprint_misses || {};
  const rows = section.rows || [];
  const content = rows.length === 0 ? [emptySection(section, data)] : [table([
    { label: "path" },
    { label: "rate", num: true },
    { label: "widened / touching", num: true, title: "Tasks whose footprint was widened onto the path ÷ tasks that touched it; repeat widenings on one task count once" },
    { label: "widenings", num: true },
  ], rows.map((row) => {
    const tr = el("tr");
    tr.appendChild(pathCell(row.path));
    for (const cell of rateCells(row)) tr.appendChild(cell);
    tr.appendChild(el("td", { class: "num", text: String(row.widenings ?? 0) }));
    return tr;
  })), belowSupportNote(section, data)];
  return sectionNode(
    "footprint_misses",
    "Footprint misses",
    "Tasks that added the path through context_files_widened ÷ tasks that touched it.",
    section,
    content,
  );
}

function couplingSection(data) {
  const section = data.change_coupling || {};
  const rows = section.rows || [];
  const content = rows.length === 0 ? [emptySection(section, data, "pair")] : [table([
    { label: "paths" },
    { label: "Jaccard", num: true },
    { label: "both / either", num: true, title: "Reservations holding both ÷ reservations holding either" },
    { label: "each", num: true, title: "Reservations holding each path" },
  ], rows.map((row) => {
    const tr = el("tr");
    const [left, right] = row.paths || [];
    tr.appendChild(el("td", { class: "codebase-path", text: `${left} ↔ ${right}`, title: `${left} ↔ ${right}` }));
    for (const cell of rateCells(row)) tr.appendChild(cell);
    tr.appendChild(el("td", { class: "num", text: (row.reservations || []).join(" · ") }));
    return tr;
  })), belowSupportNote(section, data)];
  if (Number(section.excluded_large) > 0) {
    content.push(el("div", {
      class: "codebase-note",
      text: `${section.excluded_large} reservations over ${section.large_footprint_cap} files excluded.`,
    }));
  }
  return sectionNode(
    "change_coupling",
    "Change coupling",
    "Jaccard overlap of file reservations: reservations holding both ÷ reservations holding either. Pairs below minimum support are omitted.",
    section,
    content,
  );
}

function blockedSection(data) {
  const section = data.blocked || {};
  const rows = section.rows || [];
  const causes = el("ul", { class: "codebase-causes" });
  for (const cause of section.causes || []) {
    const item = el("li", { class: `codebase-cause codebase-cause-${cause.kind}`, text: `${cause.cause.replace(/_/g, " ")} ${cause.tasks}` });
    item.dataset.cause = cause.cause;
    item.title = cause.kind === "infrastructure"
      ? "Infrastructure cause: not counted against files"
      : cause.kind === "unclassified"
        ? "Blocks with no recognisable cause"
        : "Run failures, counted against the files the task touched";
    causes.appendChild(item);
  }
  const content = [causes];
  if (rows.length === 0) content.push(emptySection(section, data));
  else {
    content.push(table([
      { label: "path" },
      { label: "rate", num: true },
      { label: "run-blocked / touching", num: true },
      { label: "unclassified", num: true, title: "Tasks blocked without a recognisable cause" },
    ], rows.map((row) => {
      const tr = el("tr");
      tr.appendChild(pathCell(row.path));
      for (const cell of rateCells(row)) tr.appendChild(cell);
      tr.appendChild(el("td", { class: "num", text: String(row.unclassified ?? 0) }));
      return tr;
    })));
    content.push(belowSupportNote(section, data));
  }
  return sectionNode(
    "blocked",
    "Blocked",
    "Tasks blocked by a run failure ÷ tasks that touched the path. Infrastructure causes (auth, sandbox, disk, provider limits) are counted by cause, not against files.",
    section,
    content,
  );
}

function rejectedSection(data) {
  const section = data.rejected || {};
  const rows = section.rows || [];
  const content = rows.length === 0 ? [emptySection(section, data)] : [table([
    { label: "path" },
    { label: "rate", num: true },
    { label: "rejected / with a run", num: true },
  ], rows.map((row) => {
    const tr = el("tr");
    tr.appendChild(pathCell(row.path));
    for (const cell of rateCells(row)) tr.appendChild(cell);
    return tr;
  })), belowSupportNote(section, data)];
  if (Number(section.excluded_without_run) > 0) {
    content.push(el("div", {
      class: "codebase-note",
      text: `${section.excluded_without_run} rejected without a run (at proposal) excluded.`,
    }));
  }
  return sectionNode(
    "rejected",
    "Rejected after a run",
    "Rejected tasks that had a job run ÷ tasks with a run that touched the path.",
    section,
    content,
  );
}

function renderCodebase(data) {
  const body = $(BODY_ID);
  if (!body || !data) return;
  const caption = el("div", { class: "codebase-caption" });
  caption.dataset.key = "caption";
  const computed = data.computed_at ? formatDateTime(data.computed_at) : "—";
  caption.appendChild(el("strong", { text: "Touched = declared footprint" }));
  caption.append(` (context_files plus widened selectors), not the landed diff · window ${data.window} · rollup ${data.rollup} · minimum support ${data.min_support} · ${data.tasks_scanned ?? 0} tasks scanned · computed ${computed}, cached ${Math.round(Number(data.cache_ttl_seconds) / 60) || 0} min`);
  caption.dataset.hash = JSON.stringify([data.window, data.rollup, data.min_support, data.tasks_scanned, data.computed_at]);
  syncNodes(body, [
    caption,
    regressionsSection(data),
    lockSection(data),
    footprintSection(data),
    couplingSection(data),
    blockedSection(data),
    rejectedSection(data),
  ]);
}

export {
  applyCodebaseHashQuery,
  codebaseHashParams,
  fetchAndRenderCodebase,
  getCodebaseQuery,
  renderCodebase,
  wireCodebaseControls,
};

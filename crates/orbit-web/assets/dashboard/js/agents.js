// Analytics › Agents: one scorecard per agent role, ranked by crew and model.
// Reads GET /api/agents/<role>/scorecard for the ranking and its terms,
// GET /api/agents/attribution for the strip of who each problem counts
// against, and GET /api/agents/<role>/detail for the tasks behind the
// selected row's biggest penalty. The server keeps one scan per workspace,
// so a role, window or row change only slices it.
//
// One renderer serves every role: the strip, the ranking, the detail column
// and the "How the score works" disclosure. A role's own panel comes from its
// module in ROLE_PANELS (js/agents-<role>.js); a role without one shows none.
// A panel may also add a banner under the strip and tags beside a row's name.
// A role whose scorecard is not computed yet shows an empty state with its
// terms. Pure vanilla JS, no build step; every node is built with `el()`.

import { el, syncNodes, fetchJson, getWindow, wireWindowSelector, requestPanel, getWorkspaceRevision, onWorkspaceChange, renderPanelPlaceholder, isAggregateView, bindTaskLink, formatDateTime } from './common.js';
import { orchestratorsPanel } from './agents-orchestrators.js';
import { reviewersPanel } from './agents-reviewers.js';
import { pilotsPanel } from './agents-pilots.js';

const $ = (id) => document.getElementById(id);

export const AGENT_ROLES = ["orchestrators", "implementers", "reviewers", "pilots"];
const ROLE_LABELS = {
  orchestrators: "Orchestrators",
  implementers: "Implementers",
  reviewers: "Reviewers",
  pilots: "Pilots",
};
// Each role's panel below the shared layout: `{ title, render(context) }`,
// where `render` returns the panel's child nodes. Optional hooks:
// `banner(context)` returns the banner's content or null, `rowTags(row)`
// returns the tags shown under a row's name, `scoreNote(row)` returns
// `{ text, detail }` shown beside the row's score (the short text in the
// ranking, the sentence in the detail column) or null, and `how` lists
// paragraphs "How the score works" adds for the role.
const ROLE_PANELS = {
  orchestrators: orchestratorsPanel,
  reviewers: reviewersPanel,
  pilots: pilotsPanel,
};
const NOT_SCORED_REASONS = {
  infrastructure: "rework caused by infrastructure",
  no_cause: "rework with no recorded cause",
  unattributed: "no orchestrator attribution",
  no_run: "no implementation run to charge",
  no_review: "no settled review to charge",
  merge_reintroduced: "regressions a merge reintroduced",
  contract_drift: "contract-drift regressions",
  test_not_selected: "regressions no selected test covered",
  platform_only: "platform-only regressions",
  nondeterministic: "nondeterministic regressions",
  unclassified: "unclassified regressions",
};
const MIN_SUPPORT = 10;
// The smoothing constant K of the shared score (`agents/score.rs`).
const SMOOTHING = 30;
const PENALTY_TASKS_SHOWN = 6;

const BODY_ID = "agents-body";
const COUNT_ID = "agents-count";
const META_ID = "agents-meta";
const TITLE_ID = "agents-title";

let role = AGENT_ROLES[0];
let requestScope = null;
let scorecard = null;
let attribution = null;
// The selected row of each role, as `rowKey(row)`.
const selected = new Map();
// The selected row's detail: its cache key, the payload once it arrived, and
// a failure to show in place.
let detail = null;
let detailRequest = 0;
const detailCache = new Map();
let countsRequest = 0;
// "How the score works" stays as the reader left it across polls and roles.
let howOpen = false;

export function getAgentsRole() {
  return role;
}

/// Show `next` on the next render; an unknown role is ignored.
export function setAgentsRole(next) {
  if (!AGENT_ROLES.includes(next) || next === role) return;
  role = next;
  requestScope = null;
  resetView();
}

function scorecardPath(forRole, selectedWindow) {
  return `/api/agents/${forRole}/scorecard?window=${encodeURIComponent(selectedWindow)}&min_support=${MIN_SUPPORT}`;
}

function attributionPath(selectedWindow) {
  return `/api/agents/attribution?window=${encodeURIComponent(selectedWindow)}`;
}

function detailPath(forRole, selectedWindow, row) {
  return `/api/agents/${forRole}/detail?window=${encodeURIComponent(selectedWindow)}&min_support=${MIN_SUPPORT}`
    + `&crew=${encodeURIComponent(row.crew)}&model=${encodeURIComponent(row.model)}`;
}

function rowKey(row) {
  return `${row.crew}\0${row.model}`;
}

function resetView() {
  scorecard = null;
  attribution = null;
  detail = null;
  detailRequest += 1;
  detailCache.clear();
  const meta = $(META_ID);
  if (meta) meta.textContent = "";
  const title = $(TITLE_ID);
  if (title) title.textContent = ROLE_LABELS[role];
}

export function placeholdAgentsAggregate() {
  requestScope = null;
  resetView();
  renderPanelPlaceholder(BODY_ID);
  const count = $(COUNT_ID);
  if (count) count.textContent = "—";
  for (const each of AGENT_ROLES) setRoleCount(each, null);
}

onWorkspaceChange(() => {
  requestScope = null;
  resetView();
});

/// Wire the shared window selector on the Agents panel.
export function wireAgentControls() {
  wireWindowSelector("agents-window-selector", { allowAll: true });
}

export function fetchAndRenderAgents() {
  if (isAggregateView()) {
    placeholdAgentsAggregate();
    return Promise.resolve();
  }
  const activeRole = role;
  const selectedWindow = getWindow();
  const revision = getWorkspaceRevision();
  const path = scorecardPath(activeRole, selectedWindow);
  const scope = `${revision}\0${path}`;
  if (requestScope !== scope) {
    requestScope = scope;
    resetView();
  }
  refreshRoleCounts(activeRole, selectedWindow, revision);
  const current = () => role === activeRole && getWorkspaceRevision() === revision && getWindow() === selectedWindow;
  return requestPanel(BODY_ID, path, () => Promise.all([fetchJson(path), fetchJson(attributionPath(selectedWindow))]), ([payload, strip]) => {
    if (!current()) return;
    if (!payload || payload.role !== activeRole || payload.requested_window !== selectedWindow) {
      console.error(`agent scorecard for ${payload && payload.role} ${payload && payload.requested_window} rejected under ${activeRole} ${selectedWindow}`);
      return;
    }
    scorecard = payload;
    attribution = strip;
    setRoleCount(activeRole, payload);
    syncSelection();
    render();
  }, COUNT_ID);
}

// Every role tab counts its ranked rows. The active role's count comes with
// its own scorecard; the others are read beside it and never block the panel.
function refreshRoleCounts(activeRole, selectedWindow, revision) {
  const request = ++countsRequest;
  for (const each of AGENT_ROLES) {
    if (each === activeRole) continue;
    fetchJson(scorecardPath(each, selectedWindow)).then((payload) => {
      if (request !== countsRequest || getWorkspaceRevision() !== revision) return;
      setRoleCount(each, payload);
    }, () => {
      if (request === countsRequest) setRoleCount(each, null);
    });
  }
}

function setRoleCount(forRole, payload) {
  const node = $(`agents-count-${forRole}`);
  if (!node) return;
  if (!payload) {
    node.textContent = "";
    node.title = "";
    return;
  }
  if (payload.status === "not_computed") {
    node.textContent = "—";
    node.title = "Not computed yet";
    return;
  }
  const rows = Array.isArray(payload.rows) ? payload.rows : [];
  const ranked = rows.filter((row) => row.rank != null).length;
  node.textContent = String(ranked);
  node.title = `${plural(ranked, "ranked row")}; ${rows.length - ranked} below ${payload.min_support} ${payload.unit}`;
}

function rows() {
  return Array.isArray(scorecard && scorecard.rows) ? scorecard.rows : [];
}

function terms() {
  return Array.isArray(scorecard && scorecard.terms) ? scorecard.terms : [];
}

function selectedRow() {
  const key = selected.get(role);
  return rows().find((row) => rowKey(row) === key) || null;
}

// Keep the reader's row while the window still has it; otherwise select the
// top of the ranking, so the detail column is never blank beside rows.
function syncSelection() {
  if (!selectedRow()) {
    const first = rows()[0];
    if (first) selected.set(role, rowKey(first));
    else selected.delete(role);
  }
  loadDetail();
}

function selectRow(row) {
  const key = rowKey(row);
  if (selected.get(role) === key) return;
  selected.set(role, key);
  loadDetail();
  render();
}

// A row's detail is cached per scan: a poll with the same `computed_at`
// reuses it, a newer scan reads it again. The previous tasks of the same row
// stay on screen while the newer ones load.
function loadDetail() {
  const row = selectedRow();
  if (!row || !scorecard || scorecard.status !== "ok") {
    detail = null;
    return;
  }
  const key = rowKey(row);
  const cacheKey = `${requestScope}\0${scorecard.computed_at}\0${key}`;
  if (detail && detail.cacheKey === cacheKey) return;
  const previous = detail && detail.rowKey === key ? detail.payload : null;
  const cached = detailCache.get(cacheKey);
  detail = { cacheKey, rowKey: key, payload: cached || previous, loading: !cached, error: null };
  if (cached) return;
  const request = ++detailRequest;
  const activeRole = role;
  fetchJson(detailPath(activeRole, getWindow(), row)).then((payload) => {
    detailCache.set(cacheKey, payload);
    if (request !== detailRequest || !detail || detail.cacheKey !== cacheKey) return;
    detail = { ...detail, payload, loading: false };
    render();
  }, (error) => {
    if (request !== detailRequest || !detail || detail.cacheKey !== cacheKey) return;
    detail = { ...detail, loading: false, error: error.message || String(error) };
    render();
  });
}

function number(value) {
  if (typeof value !== "number" || !Number.isFinite(value)) return "—";
  return Number.isInteger(value) ? String(value) : value.toFixed(1);
}

function signed(value) {
  if (typeof value !== "number" || !Number.isFinite(value)) return "—";
  const text = number(Math.abs(value));
  return value < 0 ? `−${text}` : `+${text}`;
}

function statText(value) {
  if (typeof value === "number") return number(value);
  if (typeof value === "boolean") return value ? "yes" : "no";
  return String(value ?? "—");
}

// A term weight exactly as configured: 0.25 must not read as 0.3.
function weight(value) {
  if (typeof value !== "number" || !Number.isFinite(value)) return "—";
  const text = String(Math.abs(value));
  return value < 0 ? `−${text}` : `+${text}`;
}

function plural(n, one, many = `${one}s`) {
  return `${n} ${n === 1 ? one : many}`;
}

function windowLabel() {
  if (!scorecard) return "";
  return scorecard.requested_window === scorecard.window
    ? scorecard.window
    : `${scorecard.window} (${scorecard.requested_window})`;
}

function roleHref(forRole) {
  return `#analytics/agents/${forRole}?window=${encodeURIComponent(getWindow())}`;
}

function contribution(term, row) {
  const rate = row.per_100 ? row.per_100[term.key] : null;
  return typeof rate === "number" ? term.weight * rate : null;
}

function render() {
  const body = $(BODY_ID);
  if (!body || !scorecard) return;
  const ranking = rows();
  const title = $(TITLE_ID);
  if (title) title.textContent = ROLE_LABELS[role];
  const count = $(COUNT_ID);
  if (count) count.textContent = scorecard.status === "ok" ? String(ranking.length) : "—";
  const meta = $(META_ID);
  if (meta) {
    const units = ranking.reduce((sum, row) => sum + (row.units || 0), 0);
    meta.textContent = scorecard.status === "ok"
      ? `${units} ${scorecard.unit} in ${windowLabel()} · fleet average ${number(scorecard.fleet_average)}`
        + ` · rows under ${scorecard.min_support} ${scorecard.unit} are greyed and unranked`
      : `${ROLE_LABELS[role]} are not computed yet · window ${windowLabel()}`;
  }
  const panel = ROLE_PANELS[role];
  const nodes = [stripNode()];
  const banner = panel && panel.banner && scorecard.status === "ok" ? panel.banner({ scorecard, rows: ranking, roleHref }) : null;
  if (banner) nodes.push(bannerNode(banner));
  if (scorecard.status !== "ok") {
    nodes.push(stateNode("not-computed", `${ROLE_LABELS[role]} are not computed yet`,
      `This role's scorecard is not computed yet, so it ranks no rows. "How the score works" lists the terms it will be scored on.`));
  } else if (!ranking.length) {
    nodes.push(stateNode("empty", `No ${scorecard.unit} in the ${windowLabel()} window`,
      "Widen the window to rank this role."));
  } else {
    nodes.push(rankingNode(ranking));
    const row = selectedRow();
    if (row) nodes.push(detailNode(row));
  }
  nodes.push(howNode());
  if (panel && scorecard.status === "ok" && ranking.length) nodes.push(rolePanelNode(panel));
  syncNodes(body, nodes);
}

function bannerNode(content) {
  const node = el("section", { class: "agents-banner", role: "note" }, [content]);
  node.dataset.key = "banner";
  node.dataset.hash = JSON.stringify([role, content.textContent]);
  return node;
}

function stateNode(key, heading, text) {
  const node = el("section", { class: `agents-state agents-state-${key}`, role: "status" }, [
    el("h3", { text: heading }),
    el("p", { text }),
  ]);
  node.dataset.key = "state";
  node.dataset.hash = JSON.stringify([key, heading, text]);
  return node;
}

// Who each problem counts against: one bar across the window's problems by
// owning role plus Not scored, the current role highlighted, each legend
// entry linking to its role.
function stripNode() {
  const owners = (attribution && attribution.owners) || {};
  const notScored = owners.not_scored || { count: 0, reasons: {} };
  const total = attribution && typeof attribution.total === "number" ? attribution.total : 0;
  const segments = [
    ...AGENT_ROLES.map((each) => ({ key: each, label: ROLE_LABELS[each], count: owners[each] || 0 })),
    { key: "not_scored", label: "Not scored", count: notScored.count || 0 },
  ];
  const bar = el("div", { class: "agents-strip-bar", role: "img" });
  bar.setAttribute("aria-label", segments.map((segment) => `${segment.label} ${segment.count}`).join(", "));
  for (const segment of segments) {
    if (!segment.count || !total) continue;
    const part = el("span", {
      class: `agents-strip-seg agents-owner-${segment.key}${segment.key === role ? " is-current" : ""}`,
      title: `${segment.label}: ${plural(segment.count, "problem")}`,
      style: { width: `${(segment.count * 100) / total}%` },
    });
    bar.append(part);
  }
  const reasons = Object.entries(notScored.reasons || {})
    .filter(([, count]) => count)
    .map(([reason, count]) => `${count} ${NOT_SCORED_REASONS[reason] || reason.replace(/_/g, " ")}`);
  const legend = el("ul", { class: "agents-strip-legend" });
  for (const segment of segments) {
    const swatch = el("span", { class: `agents-swatch agents-owner-${segment.key}`, "aria-hidden": "true" });
    const label = [swatch, el("span", { text: segment.label }), el("span", { class: "agents-strip-count", text: String(segment.count) })];
    let entry;
    if (segment.key === "not_scored") {
      entry = el("span", { class: "agents-strip-entry", title: reasons.length ? `Not scored: ${reasons.join(", ")}` : "Nothing went unscored" }, label);
    } else {
      entry = el("a", { class: "agents-strip-entry" }, label);
      entry.href = roleHref(segment.key);
      if (segment.key === role) entry.setAttribute("aria-current", "page");
    }
    const item = el("li", { class: segment.key === role ? "is-current" : "" }, [entry]);
    item.dataset.owner = segment.key;
    legend.append(item);
  }
  const children = [
    el("header", { class: "agents-strip-head" }, [
      el("h3", { text: "Who each problem counts against" }),
      el("span", { class: "agents-strip-total", text: attribution ? `${plural(total, "problem")} in ${windowLabel()}` : "Loading…" }),
    ]),
    total ? bar : el("p", { class: "agents-strip-none", text: "No problem was recorded in this window." }),
    legend,
  ];
  if (reasons.length) children.push(el("p", { class: "agents-strip-reasons", text: `Not scored: ${reasons.join(" · ")}.` }));
  const node = el("section", { class: "agents-strip", "aria-label": "Who each problem counts against" }, children);
  node.dataset.key = "strip";
  node.dataset.hash = JSON.stringify([attribution && attribution.owners, total, role, getWindow()]);
  return node;
}

function scoreScale(ranking) {
  const scores = ranking.map((row) => row.score).filter((score) => typeof score === "number");
  const low = Math.min(0, ...scores);
  const high = Math.max(0, ...scores);
  return { low, span: high - low || 1 };
}

// A bar from zero to `value` on the shared `scale`, so negative scores read
// left of the zero line.
function scoreBar(value, scale) {
  const zero = ((0 - scale.low) / scale.span) * 100;
  const end = ((value - scale.low) / scale.span) * 100;
  const fill = el("span", {
    class: value < 0 ? "agents-bar-fill negative" : "agents-bar-fill",
    style: { left: `${Math.min(zero, end)}%`, width: `${Math.abs(end - zero)}%` },
  });
  const track = el("span", { class: "agents-bar", "aria-hidden": "true" }, [fill]);
  if (scale.low < 0) track.append(el("span", { class: "agents-bar-zero", style: { left: `${zero}%` } }));
  return track;
}

function rankingNode(ranking) {
  const scale = scoreScale(ranking);
  const selectedKey = selected.get(role);
  const panel = ROLE_PANELS[role];
  const header = el("tr", {}, [
    el("th", { class: "agents-num", text: "Rank" }),
    el("th", { text: "Crew · model" }),
    el("th", { class: "agents-score-col", text: "Score" }),
    el("th", { class: "agents-num", text: "Support", title: `${scorecard.unit} in the window` }),
    ...terms().map((term) => el("th", {
      class: "agents-num",
      text: term.label,
      title: `${term.label} per 100 ${scorecard.unit} (weight ${weight(term.weight)}): ${term.definition}`,
    })),
  ]);
  const body = ranking.map((row) => {
    const key = rowKey(row);
    const isSelected = key === selectedKey;
    const select = el("button", {
      class: "agents-row-select",
      type: "button",
      "aria-pressed": isSelected ? "true" : "false",
      title: `Show ${row.crew} · ${row.model} in the detail column`,
    }, [
      el("span", { class: "agents-crew", text: row.crew }),
      el("span", { class: "agents-model", text: row.model }),
    ]);
    select.addEventListener("click", () => selectRow(row));
    const tags = panel && panel.rowTags ? panel.rowTags(row) : [];
    if (tags.length) select.append(el("span", { class: "agents-tags" }, tags));
    const nameCell = el("th", { class: "agents-name" }, [select]);
    nameCell.setAttribute("scope", "row");
    const support = el("td", { class: "agents-num agents-support" }, [el("span", { text: String(row.units) })]);
    if (row.below_support) support.append(el("span", { class: "agents-below", text: `below ${scorecard.min_support}` }));
    const cells = terms().map((term) => {
      const td = el("td", {
        class: "agents-num",
        text: number(row.per_100 && row.per_100[term.key]),
        title: `${term.label}: ${number(row.counts && row.counts[term.key])} (${number(row.per_100 && row.per_100[term.key])} per 100 ${scorecard.unit})`,
      });
      td.dataset.term = term.key;
      return td;
    });
    const score = el("td", { class: "agents-score-col" }, [
      scoreBar(row.score, scale),
      el("span", { class: "agents-score-value", text: number(row.score) }),
    ]);
    const note = panel && panel.scoreNote ? panel.scoreNote(row) : null;
    if (note) score.append(el("span", { class: "agents-score-note", text: note.text, title: note.detail }));
    const classes = ["agents-row"];
    if (row.below_support) classes.push("agents-row-below");
    if (isSelected) classes.push("is-selected");
    const tr = el("tr", { class: classes.join(" ") }, [
      el("td", { class: "agents-num agents-rank", text: row.rank == null ? "—" : String(row.rank) }),
      nameCell,
      score,
      support,
      ...cells,
    ]);
    tr.dataset.row = key;
    tr.addEventListener("click", () => selectRow(row));
    return tr;
  });
  const table = el("table", { class: "agents-table" }, [el("thead", {}, [header]), el("tbody", {}, body)]);
  // Rank, crew and score lead the table; the evidence columns after them scroll.
  const wrap = el("div", { class: "agents-table-wrap", role: "region", "aria-label": `${ROLE_LABELS[role]} ranking` }, [table]);
  wrap.tabIndex = 0;
  const node = el("section", { class: "agents-ranking", "aria-label": "Ranking" }, [wrap]);
  node.dataset.key = "ranking";
  node.dataset.hash = JSON.stringify([role, scorecard.rows, scorecard.terms, selectedKey]);
  return node;
}

// How the row's score came from its raw score: smoothing only pulls a row
// above the fleet average down.
function rawText(row) {
  const average = scorecard.fleet_average;
  return typeof average === "number" && row.raw > average
    ? `raw ${number(row.raw)}, pulled down toward the fleet average ${number(average)} over ${plural(row.units, "unit")}`
    : `raw ${number(row.raw)}, not above the fleet average ${number(average)}, so not pulled`;
}

function detailNode(row) {
  const panel = ROLE_PANELS[role];
  const scoreNote = panel && panel.scoreNote ? panel.scoreNote(row) : null;
  const termList = terms();
  const contributions = termList.map((term) => ({ term, value: contribution(term, row) }));
  const largest = Math.max(1e-9, ...contributions.map((entry) => Math.abs(entry.value || 0)));
  const breakdown = el("ul", { class: "agents-breakdown" });
  for (const { term, value } of contributions) {
    const width = (Math.abs(value || 0) * 50) / largest;
    const fill = el("span", {
      class: (value || 0) < 0 ? "agents-bar-fill negative" : "agents-bar-fill",
      style: (value || 0) < 0 ? { left: `${50 - width}%`, width: `${width}%` } : { left: "50%", width: `${width}%` },
    });
    const item = el("li", {}, [
      el("span", { class: "agents-term-label", text: term.label }),
      el("span", { class: "agents-term-weight", text: `× ${weight(term.weight)}`, title: "weight" }),
      el("span", { class: "agents-bar agents-bar-centered", "aria-hidden": "true" }, [fill, el("span", { class: "agents-bar-zero", style: { left: "50%" } })]),
      el("span", { class: "agents-term-value", text: signed(value), title: `${number(row.counts && row.counts[term.key])} counted, ${number(row.per_100 && row.per_100[term.key])} per 100 ${scorecard.unit}` }),
    ]);
    item.dataset.term = term.key;
    breakdown.append(item);
  }
  const stats = el("dl", { class: "agents-stats" });
  const stat = (label, value) => {
    stats.append(el("dt", { text: label }));
    stats.append(el("dd", { text: value }));
  };
  stat(`${scorecard.unit[0].toUpperCase()}${scorecard.unit.slice(1)}`, String(row.units));
  for (const entry of Array.isArray(row.stats) ? row.stats : []) {
    // Structured stats render in the role panel (the pilots' tier matrix); a
    // role without one skips them.
    if (entry.value !== null && typeof entry.value === "object") continue;
    stat(entry.label, statText(entry.value));
  }
  if (row.runs_without_model) stat("Runs with no model", String(row.runs_without_model));
  if (row.provisional != null) stat("Provisional", String(row.provisional));

  const children = [
    el("header", { class: "agents-detail-head" }, [
      el("span", { class: "agents-detail-rank", text: row.rank == null ? `below ${scorecard.min_support}` : `#${row.rank}` }),
      el("h3", { text: row.crew }),
      el("span", { class: "agents-model", text: row.model }),
    ]),
    el("div", { class: "agents-detail-score" }, [
      el("strong", { text: number(row.score) }),
      el("span", { text: "score" }),
    ]),
    el("p", { class: "agents-detail-raw", text: rawText(row) }),
    ...(scoreNote ? [el("p", { class: "agents-detail-note", text: scoreNote.detail })] : []),
    el("h4", { text: "Score breakdown" }),
    breakdown,
    penaltyNode(row, contributions),
    el("h4", { text: "Shown, not scored" }),
    stats,
  ];
  const node = el("aside", { class: "agents-detail", "aria-label": `Detail for ${row.crew} · ${row.model}` }, children);
  node.id = "agents-detail";
  node.dataset.key = "detail";
  node.dataset.hash = JSON.stringify([role, row, scorecard.terms, scorecard.fleet_average, detail && [detail.payload, detail.loading, detail.error]]);
  return node;
}

// The tasks behind the row's largest negative term, from its detail.
function penaltyNode(row, contributions) {
  let worst = null;
  for (const entry of contributions) {
    if ((entry.value || 0) < 0 && (!worst || entry.value < worst.value)) worst = entry;
  }
  const section = el("section", { class: "agents-penalty" });
  if (!worst) {
    section.append(el("h4", { text: "Biggest penalty" }));
    section.append(el("p", { class: "agents-muted", text: "No penalty in this window." }));
    return section;
  }
  section.append(el("h4", { text: `Biggest penalty: ${worst.term.label}` }));
  section.dataset.term = worst.term.key;
  const term = detail && detail.payload && Array.isArray(detail.payload.terms)
    ? detail.payload.terms.find((entry) => entry.key === worst.term.key)
    : null;
  if (detail && detail.error) {
    section.append(el("p", { class: "agents-error", role: "alert", text: `Unable to load the tasks: ${detail.error}` }));
    return section;
  }
  if (!term) {
    section.append(el("p", { class: "agents-muted", role: "status", text: "Loading the tasks…" }));
    return section;
  }
  const tasks = Array.isArray(term.tasks) ? term.tasks : [];
  section.append(el("p", { class: "agents-muted", text: `${number(term.count)} counted from ${plural(tasks.length, "task")}, ${signed(worst.value)} to the score` }));
  const list = el("ul", { class: "agents-tasks" });
  for (const task of tasks.slice(0, PENALTY_TASKS_SHOWN)) list.append(taskItem(task));
  section.append(list);
  if (tasks.length > PENALTY_TASKS_SHOWN) {
    section.append(el("p", { class: "agents-muted", text: `and ${plural(tasks.length - PENALTY_TASKS_SHOWN, "more task")}` }));
  }
  return section;
}

function taskItem(task) {
  const link = bindTaskLink(el("a", { class: "agents-task-id", text: task.task_id }), task.task_id);
  const head = el("div", { class: "agents-task-head" }, [
    link,
    el("span", { class: "agents-task-title", text: task.title || "" }),
    el("span", { class: "agents-task-meta", text: task.status || "" }),
  ]);
  if (typeof task.count === "number" && Math.abs(task.count) !== 1) head.append(el("span", { class: "agents-task-meta", text: `×${number(task.count)}` }));
  const item = el("li", { class: "agents-task" }, [head]);
  item.dataset.taskId = task.task_id;
  const occurrences = Array.isArray(task.occurrences) ? task.occurrences : [];
  if (occurrences.length) {
    const list = el("ul", { class: "agents-occurrences" });
    for (const occurrence of occurrences) {
      list.append(el("li", {}, [
        el("time", { text: formatDateTime(occurrence.at) }),
        el("span", { text: occurrence.detail || "" }),
      ]));
    }
    item.append(list);
  }
  return item;
}

function howNode() {
  const list = el("dl", { class: "agents-how-terms" });
  for (const term of terms()) {
    list.append(el("dt", {}, [
      el("span", { text: term.label }),
      el("span", { class: term.weight < 0 ? "agents-weight negative" : "agents-weight", text: weight(term.weight) }),
    ]));
    list.append(el("dd", { text: term.definition }));
  }
  const unit = scorecard.unit;
  const panel = ROLE_PANELS[role];
  const details = el("details", { class: "agents-how" }, [
    el("summary", { text: "How the score works" }),
    list,
    el("p", { text: `Each term's count becomes a rate per 100 ${unit}. The raw score is the sum of weight × rate over the terms; positive weights are credit and negative weights are problems.` }),
    el("p", { class: "agents-formula", text: `score = min(raw, (${unit} × raw + ${SMOOTHING} × fleet average) ÷ (${unit} + ${SMOOTHING}))` }),
    el("p", { text: `A row above the fleet average, the mean raw score weighted by ${unit}, is pulled down toward it, the more the less work backs it; a row below keeps its raw score. So a small row never rises on the fleet's evidence, and at an equal raw score the row with more ${unit} ranks higher. Rows with fewer than ${scorecard.min_support} ${unit} are greyed, unranked and listed last. Every problem counts against one role only, and cost and time are shown, never scored.` }),
    ...(panel && Array.isArray(panel.how) ? panel.how.map((text) => el("p", { text })) : []),
  ]);
  details.open = howOpen;
  details.addEventListener("toggle", () => { howOpen = details.open; });
  details.dataset.key = "how";
  details.dataset.hash = JSON.stringify([role, scorecard.terms, unit, scorecard.min_support]);
  return details;
}

function rolePanelNode(panel) {
  const node = el("section", { class: `agents-role-panel agents-role-panel-${role}`, "aria-label": panel.title }, [
    el("h3", { text: panel.title }),
    ...panel.render({ scorecard, rows: rows(), roleHref }),
  ]);
  node.dataset.key = "panel";
  node.dataset.hash = JSON.stringify([role, scorecard.rows]);
  return node;
}

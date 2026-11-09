// Analytics › Agents: one scorecard per agent role, ranked by model and recorded effort.
// Reads GET /api/agents/<role>/scorecard for the ranking and its terms, and
// GET /api/agents/attribution for the strip of who each problem counts
// against. The server keeps one scan per workspace, so a role or window
// change only slices it.
//
// One renderer serves every role: the ranking first, its detail column, then
// the "How the score works" and attribution disclosures. A role's own panel
// comes from its module in ROLE_PANELS (js/agents-<role>.js); a role without
// one shows none. A panel may also add a one-line banner above the ranking and
// tags beside a row's name. Explanations sit behind info hints, not on the page.
// A role whose scorecard is not computed yet shows an empty state with its
// terms. Pure vanilla JS, no build step; HTML uses `el()`, charts use inline SVG.

import { el, syncNodes, fetchJson, getWindow, setWindow, persistScopeToUrl, syncWindowSelectors, wireWindowSelector, requestPanel, getWorkspaceRevision, onWorkspaceChange, renderPanelPlaceholder, isAggregateView, formatDateTime, clockStamp, relativeTime } from './common.js';
import * as common from './common.js';
import { reviewersPanel } from './agents-reviewers.js';
import { pilotsPanel } from './agents-pilots.js';
import { implementersPanel } from './agents-implementers.js';
import { fetchAndRenderRank } from './agents-rank.js';

const $ = (id) => document.getElementById(id);

export const AGENT_ROLES = ["rank", "implementers", "reviewers", "pilots"];
// The attribution strip and the scorecard counts. Rank is a view of the
// implementer board, not an owner of problems.
const SCORECARD_ROLES = ["implementers", "reviewers", "pilots"];
const ROLE_LABELS = {
  implementers: "Implementers",
  reviewers: "Reviewers",
  pilots: "Pilots",
  rank: "Rank",
};
// Each role's panel below the shared layout: `{ title, render(context) }`,
// where `render` returns the panel's child nodes. Optional hooks:
// `hint(context)` returns the explanation behind the icon beside its title,
// `banner(context)` returns the banner's content or null, `rowTags(row)`
// returns the tags shown under a row's name, `scoreNote(row)` returns
// `{ text, detail }` shown beside the row's score (the short text in the
// ranking, the sentence in the detail column) or null, and `how` lists
// paragraphs "How the score works" adds for the role. A panel with its own
// data adds `fetch(window)`, read beside the scorecard on every fetch, and
// `hash()`, which rebuilds the panel when that data or its controls change;
// its `render` gets `rerender()` to redraw after a control changes.
// `fetch` also gets the implementer execution scope as its second argument.
const ROLE_PANELS = {
  implementers: implementersPanel,
  reviewers: reviewersPanel,
  pilots: pilotsPanel,
};
const NOT_SCORED_REASONS = {
  infrastructure: "rework caused by infrastructure",
  no_cause: "rework with no recorded cause",
  spec: "problems caused by the task's spec",
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
const BODY_ID = "agents-body";
const COUNT_ID = "agents-count";
const META_ID = "agents-meta";
const TITLE_ID = "agents-title";

let role = AGENT_ROLES[0];
let requestScope = null;
let scorecard = null;
let scanTimer = null;
let attribution = null;
let implementerMatrix = null;
let implementerTier = "";
// Which implementer tasks count by derived execution: managed (the server's
// default), all or interactive. Other roles ignore it.
const EXECUTIONS = ["managed", "all", "interactive"];
let execution = "managed";
// The ranking rejects a count window, so its window stays its own. 7d and 30d
// are also scorecard windows; 90d and all are ranking-only.
const RANK_WINDOWS = ["7d", "30d", "90d", "all"];
let rankWindow = "30d";
let rankLive = false;
// The selected row of each role, as `rowKey(row)`.
const selected = new Map();
let countsRequest = 0;
// "How the score works" and the attribution strip stay as the reader left them
// across polls and roles.
let howOpen = false;
let stripOpen = false;
// The info hint beside the implementer tier control, whose text follows its state.
let tierHint = null;

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

function executionParam(forRole) {
  return forRole === "implementers" ? `&execution=${encodeURIComponent(execution)}` : "";
}

function scorecardPath(forRole, selectedWindow) {
  return `/api/agents/${forRole}/scorecard?window=${encodeURIComponent(selectedWindow)}&min_support=${MIN_SUPPORT}${executionParam(forRole)}`;
}

function attributionPath(selectedWindow) {
  return `/api/agents/attribution?window=${encodeURIComponent(selectedWindow)}`;
}

function rowKey(row) {
  return `${row.model}\0${row.reasoning_effort ?? "provider_default"}\0${row.crew || ""}`;
}

function rowLabel(row) {
  if (row.label) return row.label;
  const effort = row.effort_label || (row.reasoning_effort === null ? "provider default" : row.reasoning_effort === "unknown" || row.reasoning_effort == null ? "effort unknown" : row.reasoning_effort);
  return `${row.model} @ ${effort}`;
}

function crewsText(row) {
  const crews = Array.isArray(row.crews) && row.crews.length ? row.crews : row.crew ? [row.crew] : [];
  return crews.join(", ");
}

function resetView() {
  clearTimeout(scanTimer);
  scanTimer = null;
  scorecard = null;
  attribution = null;
  implementerMatrix = null;
  const meta = $(META_ID);
  if (meta) meta.textContent = "";
  const title = $(TITLE_ID);
  if (title) title.textContent = ROLE_LABELS[role];
}

export function placeholdAgentsAggregate() {
  requestScope = null;
  resetView();
  renderPanelPlaceholder(BODY_ID);
  syncRankChrome();
  syncTierControl();
  const count = $(COUNT_ID);
  if (count) count.textContent = "—";
  for (const each of AGENT_ROLES) setRoleCount(each, null);
}

onWorkspaceChange(() => {
  requestScope = null;
  resetView();
});

// Scorecards default to a count of recent delivery attempts, so a quiet month
// still ranks the crews that did the work. A calendar tab hands the panel back
// to the shared window; the count itself never becomes that window.
const AGENT_COUNT_WINDOWS = ["last500", "last1000"];
const AGENT_SHARED_WINDOWS = ["7d", "30d"];
let countWindow = "last500";

function agentWindow() {
  if (countWindow) return countWindow;
  const selected = getWindow();
  return AGENT_SHARED_WINDOWS.includes(selected) ? selected : "last500";
}

/// The Agents window on screen, as its `span` link parameter names it.
export function agentsSpan() {
  if (role === "rank") return rankWindow;
  return agentWindow();
}

function setRankWindow(next) {
  if (!RANK_WINDOWS.includes(next) || next === rankWindow) return false;
  rankWindow = next;
  const selector = $("agents-rank-window-selector");
  if (selector) selector.dataset.panelWindow = next;
  return true;
}

/// Adopt the Agents window a link names in its `span` parameter: a count
/// window is the panel's own, a calendar one becomes the shared window. A link
/// without a valid `span` keeps the current selection, so a fresh visit opens
/// on the count default. `90d` and `all` belong only to Rank.
export function adoptAgentsSpan(raw) {
  if (raw === "90d" || raw === "all") {
    setRankWindow(raw);
    return;
  }
  if (raw === "7d" || raw === "30d") setRankWindow(raw);
  if (AGENT_COUNT_WINDOWS.includes(raw)) {
    if (setCountWindow(raw)) syncWindowSelectors();
    return;
  }
  if (!AGENT_SHARED_WINDOWS.includes(raw)) return;
  const leftCount = setCountWindow(null);
  const changed = setWindow(raw);
  if (changed) persistScopeToUrl();
  if (leftCount || changed) syncWindowSelectors();
}

function setCountWindow(next) {
  if (next === countWindow) return false;
  countWindow = next;
  const selector = $("agents-window-selector");
  if (selector) selector.dataset.panelWindow = next || "";
  return true;
}

/// Wire the Agents window selector: its count tabs are the panel's own, its
/// calendar tabs the shared window.
export function wireAgentControls() {
  const selector = $("agents-window-selector");
  if (selector) selector.dataset.panelWindow = countWindow || "";
  wireWindowSelector("agents-window-selector", {
    allowAll: true,
    panelWindows: AGENT_COUNT_WINDOWS,
    onPanelWindow: setCountWindow,
  });
  const rankSelector = $("agents-rank-window-selector");
  if (rankSelector) rankSelector.dataset.panelWindow = rankWindow;
  wireWindowSelector("agents-rank-window-selector", {
    windows: RANK_WINDOWS,
    panelWindows: RANK_WINDOWS,
    onPanelWindow: (next) => (next ? setRankWindow(next) : false),
  });
  wireSettleSelector();
  const executions = $("agents-execution-selector");
  if (executions) {
    executions.addEventListener("click", (event) => {
      const button = event.target.closest("[data-execution]");
      if (!button || !EXECUTIONS.includes(button.dataset.execution) || button.dataset.execution === execution) return;
      execution = button.dataset.execution;
      syncExecutionSelector();
      fetchAndRenderAgents();
    });
  }
  const tier = $("agents-tier");
  if (tier) {
    tierHint = common.pinnedInfoHint("");
    tier.after(tierHint);
    tier.addEventListener("change", () => {
      implementerTier = tier.value;
      syncSelection();
      render();
    });
  }
}

function wireSettleSelector() {
  const selector = $("agents-settle-selector");
  if (!selector || selector.dataset.wired === "true") return;
  selector.dataset.wired = "true";
  const pick = (button) => {
    if (!button) return;
    const next = button.dataset.settle === "live";
    if (next === rankLive) return;
    rankLive = next;
    syncRankChrome();
    fetchAndRenderAgents();
  };
  selector.addEventListener("click", (event) => {
    const button = event.target.closest("[data-settle]");
    if (!button || !selector.contains(button)) return;
    pick(button);
  });
  selector.addEventListener("keydown", (event) => {
    const tabs = Array.from(selector.querySelectorAll("[data-settle]"));
    const index = tabs.indexOf(event.target.closest("[data-settle]"));
    if (index < 0) return;
    let nextIndex = null;
    if (event.key === "ArrowRight") nextIndex = (index + 1) % tabs.length;
    else if (event.key === "ArrowLeft") nextIndex = (index - 1 + tabs.length) % tabs.length;
    else if (event.key === "Home") nextIndex = 0;
    else if (event.key === "End") nextIndex = tabs.length - 1;
    if (nextIndex == null) return;
    event.preventDefault();
    tabs[nextIndex].focus();
    pick(tabs[nextIndex]);
  });
}

function syncRankChrome() {
  const ranking = role === "rank" && !isAggregateView();
  const scorecardWindow = $("agents-window-control");
  if (scorecardWindow) scorecardWindow.hidden = ranking;
  const rankWindowControl = $("agents-rank-window-control");
  if (rankWindowControl) rankWindowControl.hidden = !ranking;
  const settle = $("agents-settle-control");
  if (settle) settle.hidden = !ranking;
  const provisional = $("agents-rank-provisional");
  if (provisional) provisional.hidden = !(ranking && rankLive);
  for (const button of document.querySelectorAll("#agents-rank-window-selector [data-window]")) {
    const on = button.dataset.window === rankWindow;
    button.classList.toggle("on", on);
    button.setAttribute("aria-selected", on ? "true" : "false");
    button.tabIndex = on ? 0 : -1;
  }
  for (const button of document.querySelectorAll("#agents-settle-selector [data-settle]")) {
    const on = (button.dataset.settle === "live") === rankLive;
    button.classList.toggle("on", on);
    button.setAttribute("aria-selected", on ? "true" : "false");
    button.tabIndex = on ? 0 : -1;
  }
  const body = $(BODY_ID);
  if (body && !ranking) body.classList.remove("agents-body-rank");
  syncExecutionSelector();
}

function syncExecutionSelector() {
  const control = $("agents-execution-control");
  if (control) control.hidden = role !== "implementers" && role !== "rank";
  for (const button of document.querySelectorAll("#agents-execution-selector [data-execution]")) {
    const on = button.dataset.execution === execution;
    button.classList.toggle("on", on);
    button.setAttribute("aria-selected", on ? "true" : "false");
    button.tabIndex = on ? 0 : -1;
  }
}

function syncTierControl() {
  const control = $("agents-tier-control");
  if (control) control.hidden = role !== "implementers" || isAggregateView();
  const select = $("agents-tier");
  if (select) {
    select.value = implementerTier;
    select.disabled = !implementerMatrix;
  }
  const unavailable = $("agents-tier-unavailable");
  if (unavailable) unavailable.hidden = Boolean(implementerMatrix);
  if (tierHint) {
    tierHint.firstChild.textContent = !implementerMatrix
      ? "Tier scores are unavailable. Reload the page to retry."
      : implementerTier ? "Ranks by tier crew score and support; the detail column's evidence covers all tiers."
        : "All tiers compares each row with the fleet on its own tier mix. A tier ranks by that tier's crew score.";
  }
}

export function fetchAndRenderAgents() {
  syncRankChrome();
  if (isAggregateView()) {
    placeholdAgentsAggregate();
    return Promise.resolve();
  }
  if (role === "rank") {
    const body = $(BODY_ID);
    if (body) body.classList.add("agents-body-rank");
    const activeWindow = rankWindow;
    const activeExecution = execution;
    const activeLive = rankLive;
    const revision = getWorkspaceRevision();
    const scope = `${revision}\0rank\0${activeWindow}\0${activeExecution}\0${activeLive}`;
    if (requestScope !== scope) {
      requestScope = scope;
      resetView();
    }
    refreshRoleCounts("rank", agentWindow(), revision);
    return fetchAndRenderRank({
      window: activeWindow,
      execution: activeExecution,
      live: activeLive,
      onCount: (text, title) => {
        const node = $("agents-count-rank");
        if (!node) return;
        node.textContent = text;
        node.title = title || "";
      },
    });
  }
  const activeRole = role;
  const selectedWindow = agentWindow();
  const revision = getWorkspaceRevision();
  const path = scorecardPath(activeRole, selectedWindow);
  const scope = `${revision}\0${path}`;
  if (requestScope !== scope) {
    requestScope = scope;
    resetView();
  }
  refreshRoleCounts(activeRole, selectedWindow, revision);
  const activeExecution = execution;
  const current = () => role === activeRole && getWorkspaceRevision() === revision && agentWindow() === selectedWindow
    && execution === activeExecution;
  const panel = ROLE_PANELS[activeRole];
  return requestPanel(BODY_ID, path, () => Promise.all([fetchJson(path), fetchJson(attributionPath(selectedWindow)),
    activeRole === "implementers" ? fetchJson(`/api/agents/implementers/by-type?window=${encodeURIComponent(selectedWindow)}&min_support=${MIN_SUPPORT}${executionParam(activeRole)}`).catch(() => null) : null,
    panel && panel.fetch ? panel.fetch(selectedWindow, execution) : null,
  ]), ([payload, strip, matrix]) => {
    if (!current()) return;
    if (!payload || payload.role !== activeRole || payload.requested_window !== selectedWindow
      || (activeRole === "implementers" && (payload.execution ?? "managed") !== activeExecution)) {
      console.error(`agent scorecard for ${payload && payload.role} ${payload && payload.requested_window} rejected under ${activeRole} ${selectedWindow}`);
      return;
    }
    scorecard = payload;
    attribution = strip;
    implementerMatrix = matrix?.requested_window === selectedWindow && (matrix.execution ?? "managed") === activeExecution && Array.isArray(matrix.cells) ? matrix : null;
    if (!implementerMatrix) implementerTier = "";
    setRoleCount(activeRole, payload);
    syncSelection();
    render();
  }, COUNT_ID, scanSettledNote);
}

// Every role tab counts its ranked rows. The active role's count comes with
// its own scorecard; the others are read beside it and never block the panel.
function refreshRoleCounts(activeRole, selectedWindow, revision) {
  const request = ++countsRequest;
  for (const each of SCORECARD_ROLES) {
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
  node.textContent = `${ranked} ranked`;
  const unjudged = rows.filter((row) => row.unjudged).length;
  node.title = `${plural(ranked, "ranked row")}; ${rows.length - ranked - unjudged} below ${payload.min_support} ${payload.unit}`
    + (unjudged ? `; ${unjudged} unjudged` : "");
}

function rows() {
  const all = Array.isArray(scorecard && scorecard.rows) ? scorecard.rows : [];
  if (role !== "implementers" || !implementerTier || !implementerMatrix) return all;
  const tiers = new Map();
  for (const cell of implementerMatrix.cells) {
    if (cell.tier !== implementerTier) continue;
    const key = rowKey(cell);
    const tier = tiers.get(key) || { units: 0, score: cell.crew_mean };
    tier.units += cell.n;
    tiers.set(key, tier);
  }
  const ranking = all.flatMap((row) => {
    const tier = tiers.get(rowKey(row));
    return tier?.units > 0 ? [{ ...row, allTierRow: row, units: tier.units, score: tier.score,
      below_support: tier.units < scorecard.min_support, rank: null }] : [];
  });
  ranking.sort((a, b) => Number(a.below_support) - Number(b.below_support) || b.score - a.score
    || b.units - a.units || a.crew.localeCompare(b.crew) || a.model.localeCompare(b.model));
  let nextRank = 0;
  ranking.forEach((row) => { if (!row.below_support && !row.unjudged) row.rank = ++nextRank; });
  return ranking;
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
}

function selectRow(row) {
  const key = rowKey(row);
  if (selected.get(role) === key) return;
  selected.set(role, key);
  render();
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

function statValue(row, key) {
  const entry = (Array.isArray(row.stats) ? row.stats : []).find((each) => each.key === key);
  return entry ? entry.value : undefined;
}

// Why an implementer's cost per landed task is unavailable, from its cost
// coverage over counted attempts. Complete coverage with a priced cost, or
// a row whose payload carries no cost stat, needs no note: the coverage
// stats beside it already show the sample. A valid zero cost is a number,
// so it never reaches this note.
function costUnavailableNote(row) {
  if (role !== "implementers") return null;
  const cost = statValue(row, "cost_per_weighted_landed");
  if (cost !== null) return null; // priced (including valid zero), or not reported
  const total = statValue(row, "cost_coverage_attempts");
  const measured = statValue(row, "cost_measured_attempts");
  const unpriced = statValue(row, "cost_unpriced_attempts");
  const missing = statValue(row, "cost_missing_run_attempts");
  if (![total, measured, unpriced, missing].every((value) => typeof value === "number")) {
    return "Cost per landed task is unavailable: cost coverage is not reported for this row, so priced and unpriced work cannot be told apart. It is shown only when every counted attempt is priced.";
  }
  const reasons = [];
  if (missing > 0) reasons.push(`${plural(missing, "counted attempt")} ${missing === 1 ? "has" : "have"} no run`);
  if (unpriced > 0) reasons.push(`${plural(unpriced, "counted attempt")} ${unpriced === 1 ? "has" : "have"} no priced cost`);
  const landed = row.counts && row.counts.weighted_landed;
  if (!reasons.length && !(typeof landed === "number" && landed > 0)) reasons.push("there is no landed task to divide by");
  if (!reasons.length) reasons.push("not every counted attempt is priced");
  return `Cost per landed task is unavailable because ${reasons.join(" and ")}. `
    + `Cost coverage: ${measured} measured of ${total} counted attempts. `
    + "The complete-cohort average is shown only when every counted attempt is priced; it is not averaged over priced attempts alone.";
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

// The window as the panel header names it: a count window keeps to its count,
// with how far it reaches back behind the header's hint.
function windowShortLabel() {
  return scorecard && scorecard.window_attempts ? `last ${scorecard.window_attempts} attempts` : windowLabel();
}

function windowLabel() {
  if (!scorecard) return "";
  // A count window names how far back its attempts reach.
  if (scorecard.window_attempts) {
    return `the last ${scorecard.window_attempts} delivery attempts (since ${formatDateTime(scorecard.since)})`;
  }
  return scorecard.requested_window === scorecard.window
    ? scorecard.window
    : `${scorecard.window} (${scorecard.requested_window})`;
}

// Scan freshness. A scorecard says when its scan ran (`computed_at`) and how
// long the server counts it current (`cache_ttl_seconds`). Past that window the
// server still serves the scan and may refresh it in the background; this page
// cannot see that refresh, so a stale scan is never presented as current. A
// scan stamped a little ahead of this browser's clock counts as just taken;
// further ahead, its age is not judged. A missing or malformed time or window
// makes no fresh or stale claim.
const SCAN_CLOCK_SKEW_MS = 60_000;

function scanFreshness(payload, now = Date.now()) {
  const source = payload && typeof payload === "object" ? payload : {};
  const at = typeof source.computed_at === "string" ? Date.parse(source.computed_at) : NaN;
  if (Number.isNaN(at)) return { state: "unreported" };
  const ageMs = now - at;
  if (ageMs < -SCAN_CLOCK_SKEW_MS) return { state: "ahead", at: source.computed_at };
  // Read in the past tense, so a scan within the skew reads "just now".
  const ago = relativeTime(source.computed_at, at + Math.max(0, ageMs));
  const ttl = source.cache_ttl_seconds;
  if (typeof ttl !== "number" || !Number.isFinite(ttl) || ttl < 0) return { state: "unjudged", at: source.computed_at, ago };
  return { state: ageMs > ttl * 1000 ? "stale" : "fresh", at: source.computed_at, ago, ttl };
}

const SCAN_QUALIFIERS = {
  fresh: (scan) => `Current, within its ${scan.ttl} s cache window.`,
  stale: (scan) => `Past its ${scan.ttl} s cache window; a background refresh may be running or may have failed.`,
  unjudged: () => "No cache window reported, so freshness is not judged.",
  ahead: () => "Ahead of this browser's clock, so freshness is not judged.",
};

// The header figure: the scan's time on the dashboard clock and its age. Only
// a stale scan says so on the page; the other qualifiers sit behind the hint.
function scanNode(payload) {
  clearTimeout(scanTimer);
  const node = el("span", { role: "status", "aria-live": "polite", "aria-atomic": "true" });
  const update = () => {
    const scan = scanFreshness(payload);
    node.className = `agents-scan agents-scan-${scan.state}`;
    node.dataset.freshness = scan.state;
    node.textContent = "";
    if (scan.state === "unreported") {
      node.append("Scan time not reported", common.pinnedInfoHint("Freshness is not judged."));
      return;
    }
    const time = el("time", { text: formatDateTime(scan.at) });
    time.setAttribute("datetime", scan.at);
    clockStamp(time, scan.at);
    node.append("Scan ", time, scan.state === "ahead" ? "" : ` (${scan.ago})`, scan.state === "stale" ? " · stale" : "", common.pinnedInfoHint(SCAN_QUALIFIERS[scan.state](scan)));
    // Age the retained response even while a refresh is pending or failed.
    // Refresh only this status, preserving ranks, selection and request errors.
    const untilExpiry = Date.parse(scan.at) + scan.ttl * 1000 - Date.now() + 1;
    const delay = scan.state === "fresh" ? Math.min(60_000, Math.max(1, untilExpiry)) : 60_000;
    scanTimer = setTimeout(() => {
      if (scorecard !== payload || node.parentNode !== $(META_ID)) return;
      update();
      common.updatePanelSettledNote(BODY_ID, SCAN_SETTLED[scanFreshness(payload).state]);
    }, delay);
  };
  update();
  return node;
}

// The panel's status note once a response settles, in place of "Updated.". It
// is the live region's reading of the scan; the header above shows it.
const SCAN_SETTLED = {
  fresh: "Scan current.",
  stale: "Scan stale: past its cache window.",
  unjudged: "Scan time shown; freshness not judged.",
  ahead: "Scan time ahead of this clock; freshness not judged.",
  unreported: "Scan time not reported.",
};

function scanSettledNote([payload] = []) {
  return SCAN_SETTLED[scanFreshness(payload).state];
}

function roleHref(forRole) {
  return `#analytics/agents/${forRole}?window=${encodeURIComponent(getWindow())}&span=${encodeURIComponent(agentWindow())}`;
}

// A term rated per 100 of its own unit (the pilots' warning terms, per 100
// settled assessments) names that unit; every other term uses the role's.
function termUnit(term) {
  return term.unit || scorecard.unit;
}

// A term below its own minimum support on the row: rated and shown, but
// left out of the score.
function provisionalTerm(term, row) {
  return Array.isArray(row.provisional_terms) && row.provisional_terms.includes(term.key);
}

function contribution(term, row) {
  if (provisionalTerm(term, row) || noReviewedDeliveries(term, row)) return 0;
  const rate = row.per_100 ? row.per_100[term.key] : null;
  const expected = row.expected && typeof row.expected[term.key] === "number" ? row.expected[term.key] * 100 / Math.max(1, row.term_units?.[term.key] ?? row.units) : 0;
  return typeof rate === "number" ? term.weight * (rate - expected) : null;
}

function render() {
  syncRankChrome();
  syncTierControl();
  const body = $(BODY_ID);
  if (!body || !scorecard) return;
  const ranking = rows();
  const title = $(TITLE_ID);
  if (title) title.textContent = ROLE_LABELS[role];
  const count = $(COUNT_ID);
  if (count) {
    const ranked = ranking.filter((row) => row.rank != null).length;
    count.textContent = scorecard.status === "ok" ? `${ranking.length} rows · ${ranked} ranked` : "—";
  }
  const meta = $(META_ID);
  if (meta) {
    const units = ranking.reduce((sum, row) => sum + (row.units || 0), 0);
    meta.textContent = scorecard.status === "ok"
      ? `${units} ${scorecard.unit} in ${windowShortLabel()} · fleet average ${number(role === "implementers" && implementerTier ? 100 : scorecard.fleet_average)}`
      : `${ROLE_LABELS[role]} are not computed yet · window ${windowLabel()}`;
    if (scorecard.status === "ok") {
      const notes = [
        ...(scorecard.window_attempts ? [`Window: ${windowLabel()}.`] : []),
        `Rows under ${scorecard.min_support} ${scorecard.unit} are greyed and unranked.`,
      ];
      if (ranking.some((row) => row.unjudged)) notes.push("unjudged rows are listed last, without a score.");
      if (role === "implementers") notes.push(`Counting ${EXECUTION_LABELS[scorecard.execution] || "managed"} tasks.`);
      meta.append(common.pinnedInfoHint(notes.join("\n")));
    }
    meta.append(" · ", scanNode(scorecard));
  }
  const panel = ROLE_PANELS[role];
  const nodes = [];
  const banner = panel && panel.banner && scorecard.status === "ok" ? panel.banner({ scorecard, rows: ranking, roleHref }) : null;
  if (banner) nodes.push(bannerNode(banner));
  if (scorecard.status !== "ok") {
    nodes.push(stateNode("not-computed", `${ROLE_LABELS[role]} are not computed yet`,
      `This role's scorecard is not computed yet, so it ranks no rows. "How the score works" lists the terms it will be scored on.`));
  } else if (!ranking.length) {
    nodes.push(stateNode("empty", scorecard.window_attempts
      ? `No ${scorecard.unit} in ${windowLabel()}`
      : `No ${scorecard.unit} in the ${windowLabel()} window`,
      role === "implementers" && implementerTier ? "Choose another tier or widen the window to find work." : "Widen the window to rank this role."));
  } else {
    nodes.push(rankingNode(ranking));
    const row = selectedRow();
    if (row) nodes.push(detailNode(row));
  }
  // Compare, then see what is behind the ranks; how it is scored is reference.
  if (panel && scorecard.status === "ok" && ranking.length) nodes.push(rolePanelNode(panel));
  nodes.push(howNode(), stripNode());
  if (role === "implementers" && scorecard.status === "ok") nodes.push(executionSplitNode());
  syncNodes(body, nodes);
}

const EXECUTION_LABELS = { managed: "managed", all: "managed and interactive", interactive: "interactive" };

// Interactive work per crew and model, whatever the execution scope, so the
// tasks the default leaves out stay visible: each row's managed, interactive
// and unclassified tasks with the interactive ones' outcomes.
function executionSplitNode() {
  const split = Array.isArray(scorecard.execution_split) ? scorecard.execution_split : [];
  const shown = split.filter((row) => row.interactive_tasks || row.unclassified_tasks);
  const content = shown.length
    ? el("div", { class: "agents-table-wrap", role: "region", "aria-label": "Interactive work by crew" }, [
      el("table", { class: "agents-table" }, [
        el("thead", {}, [el("tr", {}, [
          el("th", { text: "Model · effort" }),
          el("th", { class: "agents-num", text: "Managed", title: "Tasks a job worked" }),
          el("th", { class: "agents-num", text: "Interactive", title: "Tasks that reached review or done without a job" }),
          el("th", { class: "agents-num", text: "Interactive landed" }),
          el("th", { class: "agents-num", text: "Interactive regressed", title: "Interactive tasks a regression_from link names" }),
          el("th", { class: "agents-num", text: "Unclassified", title: "Worked without a job and not yet in review: counted in neither population" }),
        ])]),
        el("tbody", {}, shown.map((row) => el("tr", {}, [
          el("td", {}, [el("span", { class: "agents-crew", text: rowLabel(row) }), " ", el("span", { class: "agents-model", text: crewsText(row) })]),
          el("td", { class: "agents-num", text: String(row.managed_tasks) }),
          el("td", { class: "agents-num", text: String(row.interactive_tasks) }),
          el("td", { class: "agents-num", text: String(row.interactive_landed) }),
          el("td", { class: "agents-num", text: String(row.interactive_regressed) }),
          el("td", { class: "agents-num", text: String(row.unclassified_tasks) }),
        ]))),
      ]),
    ])
    : el("p", { class: "agents-execution-none", text: `No interactive work in ${windowLabel()}: every counted task was worked by a job.` });
  if (shown.length) content.tabIndex = 0;
  const node = el("section", { class: "agents-execution", "aria-label": "Interactive work by crew" }, [
    el("h3", {}, ["Interactive work by crew", common.pinnedInfoHint("Tasks worked in an interactive session have no job run, so no cost, tokens or run time. They are left out of the counts above unless the execution control includes them; cost and run time always count managed tasks only.")]),
    content,
  ]);
  node.dataset.key = "execution";
  node.dataset.hash = JSON.stringify([split, scorecard.since]);
  return node;
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

// Who each problem counts against, a collapsed disclosure under the ranking:
// one bar across the window's problems by owning role plus Not scored, the
// current role highlighted, each legend entry linking to its role.
function stripNode() {
  const owners = (attribution && attribution.owners) || {};
  const notScored = owners.not_scored || { count: 0, reasons: {} };
  const total = attribution && typeof attribution.total === "number" ? attribution.total : 0;
  const segments = [
    ...SCORECARD_ROLES.map((each) => ({ key: each, label: ROLE_LABELS[each], count: owners[each] || 0 })),
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
  const stripContext = common.pinnedInfoHint("Ownership counts include unscored context; rankings use only each role's scored terms.");
  stripContext.id = "agents-strip-context";
  stripContext.setAttribute("role", "note");
  const legend = el("ul", { class: "agents-strip-legend" });
  for (const segment of segments) {
    const swatch = el("span", { class: `agents-swatch agents-owner-${segment.key}`, "aria-hidden": "true" });
    const label = [swatch, el("span", { text: segment.label }), el("span", { class: "agents-strip-count", text: String(segment.count) })];
    let entry;
    if (segment.key === "not_scored") {
      entry = el("span", { class: "agents-strip-entry", title: reasons.length ? `Not scored: ${reasons.join(", ")}` : "Nothing went unscored" }, [
        ...label,
        ...(reasons.length ? [el("span", { class: "visually-hidden", text: reasons.join(" · ") })] : []),
      ]);
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
    el("summary", { class: "agents-strip-head" }, [
      el("h3", {}, ["Who each problem counts against", stripContext]),
      el("span", { class: "agents-strip-total", text: attribution ? `${plural(total, "problem")} in ${windowLabel()}` : "Loading…" }),
    ]),
    total ? bar : el("p", { class: "agents-strip-none", text: "No problem was recorded in this window." }),
    legend,
  ];
  const node = el("details", { class: "agents-strip agents-disclosure", "aria-label": "Who each problem counts against", "aria-describedby": "agents-strip-context" }, children);
  node.open = stripOpen;
  node.addEventListener("toggle", () => { stripOpen = node.open; });
  node.dataset.key = "strip";
  node.dataset.hash = JSON.stringify([attribution && attribution.owners, total, role, agentWindow()]);
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

const SCORE_HINT = "One row per model and recorded reasoning effort (crews that contributed are listed under the row; unrecorded effort is its own effort-unknown row and a null effort is the provider default), ranked by one shared score. Every problem counts against exactly one role, or is not scored; cost and time are shown beside the score, never in it. Each term and the formula are under How the score works.";

// The ranking's term columns. A review term no row can score (implementers,
// no reviewed deliveries anywhere) has nothing to show, so it leaves the
// table; `hidden` lists those terms for the one note that says why.
function rankingColumns(ranking) {
  const all = role === "implementers" && implementerTier ? [] : terms();
  if (!ranking.length) return { shown: all, hidden: [] };
  const hidden = all.filter((term) => ranking.every((row) => noReviewedDeliveries(term, row)));
  return { shown: all.filter((term) => !hidden.includes(term)), hidden };
}

function hiddenColumnsNote(hidden) {
  const labels = hidden.map((term) => term.label);
  const list = labels.length > 1 ? `${labels.slice(0, -1).join(", ")} and ${labels[labels.length - 1]}` : labels[0];
  const note = el("p", {
    class: "agents-ranking-note",
    text: `${list} ${hidden.length > 1 ? "are" : "is"} not shown: no row has a reviewed delivery in this window, so none can be scored.`,
  });
  return note;
}

function rankingNode(ranking) {
  const scale = scoreScale(ranking);
  const columns = rankingColumns(ranking);
  const selectedKey = selected.get(role);
  const panel = ROLE_PANELS[role];
  const header = el("tr", {}, [
    el("th", { class: "agents-num", text: "Rank" }),
    el("th", { text: "Model · effort" }),
    el("th", { class: "agents-score-col" }, [el("span", { text: "Score" }), common.pinnedInfoHint(SCORE_HINT)]),
    el("th", { class: "agents-num", text: "Support", title: `${scorecard.unit} in the window` }),
    ...columns.shown.map((term) => el("th", {
      class: "agents-num",
      text: term.label,
      title: `${term.label} per 100 ${termUnit(term)} (weight ${weight(term.weight)}): ${term.definition}`,
    })),
  ]);
  const body = ranking.map((row) => {
    const key = rowKey(row);
    const isSelected = key === selectedKey;
    const select = el("button", {
      class: "agents-row-select",
      type: "button",
      "aria-pressed": isSelected ? "true" : "false",
      title: `Show ${rowLabel(row)} in the detail column. Crews: ${crewsText(row)}. ${effortSummary(row)}`,
    }, [
      el("span", { class: "agents-crew", text: rowLabel(row) }),
      el("span", { class: "agents-model", text: crewsText(row) }),
    ]);
    select.addEventListener("click", () => selectRow(row));
    const tags = [...(row.unjudged ? [unjudgedTag()] : []), ...(panel && panel.rowTags ? panel.rowTags(row) : [])];
    if (tags.length) select.append(el("span", { class: "agents-tags" }, tags));
    const nameCell = el("th", { class: "agents-name" }, [select]);
    nameCell.setAttribute("scope", "row");
    const support = el("td", { class: "agents-num agents-support" }, [el("span", { text: String(row.units) })]);
    if (row.below_support) support.append(el("span", { class: "agents-below", text: `below ${scorecard.min_support}` }));
    const cells = columns.shown.map((term) => {
      const provisional = provisionalTerm(term, row);
      const noReviews = noReviewedDeliveries(term, row);
      const td = el("td", {
        class: provisional ? "agents-num agents-term-provisional" : "agents-num",
        text: noReviews ? "—" : number(row.per_100 && row.per_100[term.key]),
        title: noReviews ? `${term.label}: no reviewed deliveries; not scored` : `${term.label}: ${number(row.counts && row.counts[term.key])} (${number(row.per_100 && row.per_100[term.key])} per 100 ${termUnit(term)})`
          + (provisional ? `; provisional, too few ${termUnit(term)} to score` : ""),
      });
      td.dataset.term = term.key;
      if (noReviews) td.setAttribute("aria-label", `${term.label}: no reviewed deliveries`);
      return td;
    });
    const score = el("td", { class: "agents-score-col" }, row.unjudged
      ? [el("span", { class: "agents-score-value agents-unjudged", text: "unjudged" })]
      : [scoreBar(row.score, scale), el("span", { class: "agents-score-value", text: number(row.score) })]);
    const note = panel && panel.scoreNote ? panel.scoreNote(row) : null;
    if (note) score.append(el("span", { class: "agents-score-note", text: note.text, title: note.detail }));
    const classes = ["agents-row"];
    if (row.below_support || row.unjudged) classes.push("agents-row-below");
    if (row.unjudged) classes.push("agents-row-unjudged");
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
  wrap.addEventListener("keydown", (event) => {
    if (event.target !== wrap || (event.key !== "ArrowLeft" && event.key !== "ArrowRight")) return;
    const step = Math.max(40, Math.floor(wrap.clientWidth * 0.8));
    const maximum = wrap.scrollWidth - wrap.clientWidth;
    const next = Math.max(0, Math.min(maximum, wrap.scrollLeft + (event.key === "ArrowRight" ? step : -step)));
    if (next === wrap.scrollLeft) return;
    wrap.scrollLeft = next;
    event.preventDefault();
  });
  const node = el("section", { class: "agents-ranking", "aria-label": "Ranking" }, [...(columns.hidden.length ? [hiddenColumnsNote(columns.hidden)] : []), wrap]);
  node.dataset.key = "ranking";
  node.dataset.hash = JSON.stringify([role, ranking, scorecard.terms, selectedKey, implementerTier]);
  return node;
}

// The tag beside an unjudged row's name: none of its units is judged yet,
// so it has no score or rank.
function unjudgedTag() {
  const tag = el("span", {
    class: "agents-tag agents-tag-unjudged",
    text: "unjudged",
    title: "No unit of this row is judged yet, so it has no score or rank and stays out of the fleet average.",
  });
  tag.dataset.tag = "unjudged";
  return tag;
}

// How the row's score came from its raw score: smoothing only pulls a row
// above the fleet average down.
function rawText(row) {
  if (row.unjudged) return "Not judged yet: no unit of this row has an outcome, so it has no score or rank and stays out of the fleet average.";
  const average = scorecard.fleet_average;
  return typeof average === "number" && row.raw > average
    ? `raw ${number(row.raw)}, pulled down toward the fleet average ${number(average)} over ${plural(row.units, "unit")}`
    : `raw ${number(row.raw)}, not above the fleet average ${number(average)}, so not pulled`;
}

// Terms whose rate needs a settled review behind the row (implementers).
const REVIEW_RATE_TERMS = new Set(["first_pass_accepts", "review_rejects", "reviewer_repairs"]);
// Reviewed deliveries behind a row, when the payload carries them:
// `reviewed_units` (implementers). `null` when
// the payload carries no reviewed denominator at all.
function reviewedUnits(each) {
  if (typeof each.reviewed_units === "number") return each.reviewed_units;
  return null;
}

function noReviewedDeliveries(term, row) {
  return role === "implementers" && REVIEW_RATE_TERMS.has(term.key) && reviewedUnits(row) === 0;
}

// The radar is a percentile view of rates: outward is better, the fleet
// median always sits on the middle ring, and each vertex discloses its
// term weight and signed contribution versus that median.
function radarNode(row) {
  const axes = terms().map((term) => ({ ...term }));
  // Only count-valued context with a clear better direction may extend a
  // short role. Never turn hours, currency, percentages or matrices into axes.
  for (const key of ["unclassified", "dropped_initiative", "unknown_origin", "rework_infrastructure", "rework_no_cause", "regressions_unclassified"]) {
    if (axes.length >= 5) break;
    const stat = (row.stats || []).find((entry) => entry.key === key);
    if (stat && Number.isFinite(stat.value) && !axes.some((axis) => axis.key === key)) {
      axes.push({ key, label: stat.label.replace(/ \(not scored\)$/, ""), weight: -1, unscored: true });
    }
  }
  const below = (each) => each.below_support || each.unjudged || each.units < scorecard.min_support;
  const count = (axis, each) => axis.unscored
    ? (each.stats || []).find((stat) => stat.key === axis.key)?.value
    : each.counts && each.counts[axis.key];
  const units = (axis, each) => each.term_units?.[axis.key] ?? each.units;
  const rate = (axis, each) => axis.unscored
    ? (each.units > 0 && Number.isFinite(count(axis, each)) ? count(axis, each) * 100 / each.units : null)
    : each.per_100 && each.per_100[axis.key];
  const gap = (axis, each) => {
    if (each.unjudged) return "not judged yet";
    if (below(each)) return "too little evidence to compare";
    if (noReviewedDeliveries(axis, each)) return "no reviewed deliveries";
    if (provisionalTerm(axis, each) || (axis.key === "missed_regressions" && each.provisional > 0)) return "provisional";
    if (axis.key === "missed_scope_blocks" && scorecard.sources?.missed_scope_blocks?.available === false) return "unmeasured";
    if (!(units(axis, each) > 0)) return "no units";
    if (axis.unit && !(role === "implementers" && REVIEW_RATE_TERMS.has(axis.key)) && units(axis, each) < (scorecard.warning_min_support ?? scorecard.min_support)) return "below term support";
    return Number.isFinite(rate(axis, each)) ? null : "no rate";
  };
  const fleet = (scorecard.rows || []).filter((each) => each.rank != null && !below(each));
  const values = axes.map((axis) => {
    const rates = fleet.filter((each) => !gap(axis, each)).map((each) => rate(axis, each)).sort((a, b) => a - b);
    const middle = Math.floor(rates.length / 2);
    const median = rates.length ? (rates[middle] + rates[Math.floor((rates.length - 1) / 2)]) / 2 : null;
    // Percentile in the supported fleet, median anchored at 0.5: an
    // outlier only occupies the outer percentile steps, so it cannot
    // stretch a span and squeeze every normal crew toward one end.
    const scale = (value) => {
      if (!Number.isFinite(value) || !rates.length) return null;
      if (value === median) return 0.5;
      const under = rates.filter((each) => each < value).length;
      const equal = rates.filter((each) => each === value).length;
      const percentile = (under + equal / 2) / rates.length;
      return axis.weight < 0 ? 1 - percentile : percentile;
    };
    const reason = gap(axis, row) || (!rates.length ? "no supported fleet comparison" : null);
    const ownRate = rate(axis, row);
    // Signed contribution versus the fleet median: weight × (rate −
    // median). Positive helps the score relative to the median, negative
    // hurts it, whichever direction the axis points.
    const contributionVsMedian = !axis.unscored && !reason && Number.isFinite(ownRate) && median != null
      ? axis.weight * (ownRate - median)
      : null;
    return { axis, median, reason, own: reason ? null : scale(ownRate), fleet: rates.length ? 0.5 : null, contributionVsMedian };
  });
  const svg = (tag, attrs = {}, text) => {
    const node = document.createElementNS("http://www.w3.org/2000/svg", tag);
    for (const [name, value] of Object.entries(attrs)) node.setAttribute(name, String(value));
    if (text != null) node.textContent = text;
    return node;
  };
  const point = (index, value) => {
    const angle = -Math.PI / 2 + index * 2 * Math.PI / axes.length;
    return [150 + Math.cos(angle) * 104 * value, 130 + Math.sin(angle) * 104 * value];
  };
  const plot = svg("svg", { viewBox: "0 0 300 260", role: "group", "aria-label": `Agent profile for ${rowLabel(row)} versus ranked fleet median; outward is better` });
  for (const level of [0.25, 0.5, 0.75, 1]) {
    plot.append(svg("polygon", { points: axes.map((_, i) => point(i, level).join(",")).join(" "), class: "agents-radar-grid" }));
  }
  values.forEach(({ axis }, i) => {
    const [x, y] = point(i, 1);
    plot.append(svg("line", { x1: 150, y1: 130, x2: x, y2: y, class: `agents-radar-spoke${axis.unscored ? " is-unscored" : ""}` }));
  });
  for (const series of ["fleet", "own"]) {
    const points = values.map((value, i) => value[series] == null ? null : point(i, value[series]));
    if (points.every(Boolean)) {
      plot.append(svg("polygon", { points: points.map((p) => p.join(",")).join(" "), class: `agents-radar-${series}` }));
    } else {
      // Never close or fill across absent evidence, including the wraparound.
      points.forEach((p, i) => {
        const next = points[(i + 1) % points.length];
        if (p && next) plot.append(svg("line", { x1: p[0], y1: p[1], x2: next[0], y2: next[1], class: `agents-radar-${series}` }));
      });
    }
    points.forEach((p) => {
      if (p) plot.append(svg("circle", { cx: p[0], cy: p[1], r: 3, class: `agents-radar-${series}` }));
    });
  }
  const tooltip = el("p", { id: "agents-radar-tooltip", class: "agents-radar-tooltip", role: "status", text: "Hover or focus a vertex for its evidence." });
  const tableBody = el("tbody");
  const heading = (text, scope) => {
    const node = el("th", { text });
    node.setAttribute("scope", scope);
    return node;
  };
  const labels = el("ol", { class: "agents-radar-axes" });
  values.forEach(({ axis, median, own, reason, contributionVsMedian }, i) => {
    const label = `${axis.label}${axis.weight < 0 ? " (fewer is better)" : " (more is better)"}${axis.unscored ? " · unscored" : ""}`;
    const ownText = reason ? "—" : `${number(rate(axis, row))} per 100 ${termUnit(axis)}`;
    const fleetText = median == null ? "unavailable" : `${number(median)} per 100 ${termUnit(axis)}`;
    const weightText = axis.unscored ? "unscored" : weight(axis.weight);
    const contributionText = axis.unscored ? "unscored" : contributionVsMedian == null ? "—" : signed(contributionVsMedian);
    const evidence = `${number(count(axis, row))} raw; ${number(units(axis, row))} ${termUnit(axis)}`;
    const info = `${label}: agent ${ownText}; fleet median ${fleetText}; weight ${weightText}; contribution vs fleet median ${contributionText}; ${evidence}${reason ? `; gap: ${reason}` : ""}.`;
    const [x, y] = point(i, own ?? 1);
    const vertex = svg("g", { tabindex: 0, role: "button", "aria-label": info, "aria-describedby": "agents-radar-tooltip", class: `agents-radar-vertex${reason ? " is-gap" : ""}` });
    vertex.dataset.term = axis.key;
    vertex.append(svg("circle", { cx: x, cy: y, r: 12, class: "agents-radar-hit" }));
    vertex.append(svg("circle", { cx: x, cy: y, r: 4, class: "agents-radar-marker" }));
    // At the centre, several genuine zero-radius values can coincide. The
    // numbered endpoint is another hover target for each of those vertices.
    const [lx, ly] = point(i, 1.16);
    vertex.append(svg("text", { x: lx, y: ly, "text-anchor": "middle", "dominant-baseline": "central", class: "agents-radar-label" }, String(i + 1)));
    vertex.append(svg("title", {}, info));
    const disclose = () => { tooltip.textContent = info; };
    for (const event of ["mouseenter", "focus", "click"]) vertex.addEventListener(event, disclose);
    plot.append(vertex);
    labels.append(el("li", { class: axis.unscored ? "is-unscored" : "", text: label }));
    const tableRow = el("tr", {}, [
      heading(label, "row"),
      ...[ownText, fleetText, weightText, contributionText, evidence, reason || "supported"].map((text) => el("td", { text })),
    ]);
    tableRow.dataset.term = axis.key;
    tableBody.append(tableRow);
  });
  // A table ignores overflow and its nowrap columns set a minimum width, so
  // the offscreen profile sits in a 1px scroll container. Its columns stay in
  // the accessibility tree and out of the Analytics pane's scroll range.
  const table = el("table", {}, [
    el("caption", { text: `Agent profile: ${rowLabel(row)}; ranked fleet in ${windowLabel()}` }),
    el("thead", {}, [el("tr", {}, ["Axis", "Agent rate", "Fleet median rate", "Weight", "Contribution vs fleet median", "Raw count and units", "Evidence"].map((text) => heading(text, "col")))]),
    tableBody,
  ]);
  const profile = el("div", { class: "visually-hidden" }, [table]);
  return el("section", { class: `agents-radar${below(row) ? " is-muted" : ""}`, "aria-label": "Agent profile" }, [
    el("h4", {}, ["Agent profile", common.pinnedInfoHint("Outward is better. Each axis is the row's percentile in the ranked fleet; the dashed fleet median always sits on the middle ring. Weight and contribution vs the median explain each axis. Hover a vertex or its number, or focus it, for rates and evidence. Hollow outer markers are gaps, not values.")]),
    ...(below(row) ? [el("p", { class: "agents-radar-note", text: row.unjudged ? "Nothing to compare: no unit of this row is judged yet." : "Too little evidence to compare: below minimum support." })] : []),
    plot,
    el("p", { class: "agents-radar-legend" }, [el("span", { class: "agents-radar-own-key", text: "Selected agent" }), el("span", { class: "agents-radar-fleet-key", text: "Fleet median" })]),
    labels, tooltip, profile,
  ]);
}

function detailNode(rankedRow) {
  const row = rankedRow.allTierRow || rankedRow;
  const tierView = Boolean(rankedRow.allTierRow);
  const panel = ROLE_PANELS[role];
  const scoreNote = panel && panel.scoreNote ? panel.scoreNote(row) : null;
  const termList = terms();
  const contributions = termList.map((term) => ({ term, value: contribution(term, row) }));
  const largest = Math.max(1e-9, ...contributions.map((entry) => Math.abs(entry.value || 0)));
  const breakdown = el("ul", { class: "agents-breakdown" });
  if (typeof row.score_base === "number") {
    breakdown.append(el("li", {}, [
      el("span", { class: "agents-term-label", text: "Fleet reference" }),
      el("span"), el("span"),
      el("span", { class: "agents-term-value", text: signed(row.score_base) }),
    ]));
  }
  for (const { term, value } of contributions) {
    const width = (Math.abs(value || 0) * 50) / largest;
    const fill = el("span", {
      class: (value || 0) < 0 ? "agents-bar-fill negative" : "agents-bar-fill",
      style: (value || 0) < 0 ? { left: `${50 - width}%`, width: `${width}%` } : { left: "50%", width: `${width}%` },
    });
    const label = el("span", { class: "agents-term-label", text: term.label });
    if (row.expected && typeof row.expected[term.key] === "number") {
      label.append(el("small", { class: "agents-term-context", text: `${number(row.counts[term.key])} actual · ${number(row.expected[term.key])} expected` }));
    }
    const ownUnits = row.term_units && typeof row.term_units[term.key] === "number" ? row.term_units[term.key] : null;
    if (ownUnits != null) {
      const provisional = provisionalTerm(term, row);
      const context = el("small", {
        class: provisional ? "agents-term-context agents-term-provisional" : "agents-term-context",
        text: noReviewedDeliveries(term, row) ? "no reviewed deliveries · not scored" : `${number(row.counts && row.counts[term.key])} of ${plural(ownUnits, termUnit(term).replace(/s$/, ""), termUnit(term))}${provisional ? " · provisional, not scored" : ""}`,
      });
      label.append(context);
    }
    const item = el("li", {}, [
      label,
      el("span", { class: "agents-term-weight", text: `× ${weight(term.weight)}`, title: "weight" }),
      el("span", { class: "agents-bar agents-bar-centered", "aria-hidden": "true" }, [fill, el("span", { class: "agents-bar-zero", style: { left: "50%" } })]),
      el("span", { class: "agents-term-value", text: signed(value), title: `${number(row.counts && row.counts[term.key])} counted, ${number(row.per_100 && row.per_100[term.key])} per 100 ${termUnit(term)}` }),
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
  const costNote = costUnavailableNote(row);

  const children = [
    el("header", { class: "agents-detail-head" }, [
      el("span", { class: "agents-detail-rank", text: rankedRow.unjudged ? "unjudged" : rankedRow.rank == null ? `below ${scorecard.min_support}` : `#${rankedRow.rank}` }),
      el("h3", { text: rowLabel(row) }),
      el("span", { class: "agents-model", text: crewsText(row) }),
      el("span", { class: "agents-model agents-effort", text: effortSummary(row) }),
    ]),
    el("div", { class: "agents-detail-score" }, [
      el("strong", { text: number(rankedRow.score) }),
      el("span", { text: rankedRow.unjudged ? "no score until a unit is judged" : tierView ? `${implementerTier} tier crew score` : "score" }),
    ]),
    ...(tierView ? [el("p", { class: "agents-detail-note" }, [
      `${rankedRow.units} tasks in this tier · all tiers score ${number(row.score)}`,
      common.pinnedInfoHint("Tier crew scores use two-sided shrinkage toward 100 with 30 tasks of reference evidence. The profile, breakdown and unscored context below cover all tiers."),
    ])] : []),
    el("p", { class: "agents-detail-raw", text: `${tierView ? "All tiers: " : ""}${rawText(row)}` }),
    ...(scoreNote ? [el("p", { class: "agents-detail-note" }, [el("span", { text: scoreNote.text }), common.pinnedInfoHint(scoreNote.detail)])] : []),
    radarNode(row),
    el("h4", { text: tierView ? "All tiers score breakdown" : "Score breakdown" }),
    breakdown,
    el("h4", { text: tierView ? "Shown, not scored · all tiers" : "Shown, not scored" }),
    stats,
    ...(costNote ? [el("p", { class: "agents-detail-note agents-cost-note", text: costNote })] : []),
  ];
  const node = el("aside", { class: "agents-detail", "aria-label": `Detail for ${rowLabel(row)}` }, children);
  node.id = "agents-detail";
  node.dataset.key = "detail";
  node.dataset.hash = JSON.stringify([role, rankedRow, implementerTier, scorecard.rows, scorecard.terms, scorecard.min_support, scorecard.warning_min_support, scorecard.sources, scorecard.fleet_average]);
  return node;
}

// The roles whose terms weigh regressions or findings by severity.
const SEVERITY_ROLES = new Set(["reviewers"]);
const SEVERITY_HOW = "Regressions and findings count by severity: critical 8, high 4, medium 2, low 1, nit 0.25, so one missed regression costs reviewers twice its weight, from −16 for critical to −0.5 for nit, and one escaped regression costs implementers half its weight, from −4 to −0.125. A review-job finding counts once finding verification CONFIRMS it, at the severity the verifier rated, or, with no verification verdict, once its task lands, at the highest severity rated on its own regression links, else low; a review gate finding counts at the reviewer's own. A regression link counts at its recorded severity, which verification replaces when it confirms the finding that filed it, and a link stored without one counts as medium.";

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
  const details = el("details", { class: "agents-how agents-disclosure" }, [
    el("summary", { text: "How the score works" }),
    list,
    el("p", { text: role === "implementers"
      ? `The raw score starts at 100, then adds weight × (actual − expected) per 100 ${unit}, except first-pass accepts, review rejects and reviewer repairs, which use reviewed deliveries. Review expectations use only the row's reviewed tier mix, and no reviewed deliveries contributes nothing. Expected counts use the fleet's rates on the corresponding tier mix. Tiers with fewer than 30 fleet tasks pool with the largest tier. Landings count one each at every complexity. Tasks tagged no-diff-expected when an attempt was admitted are not implementer work and count nowhere here.`
      : `Each term's count becomes a rate per 100 ${unit}. The raw score is the sum of weight × rate over the terms; positive weights are credit and negative weights are problems.` }),
    ...(role === "implementers" ? [el("p", { text: "Escaped regressions are scored: regression links with escape no_test, mostly delivery-review findings, at their severity weights (critical 8, high 4, medium 2, low 1, nit 0.25), weighted −0.5. Change size dominates regression risk, so their expected count uses fleet rates within the same tier and expected-size bucket. Expected size is the fleet median landed lines changed for the task's tier and type, never its own diff. Buckets double in size (under 2 lines, 2–4, 4–8, and so on); missing medians share an unknown-size bucket in their tier, without pooling tiers. A defect a pre-landing review already rejected or repaired counts once, as that verdict." }),
      el("p", { text: "Diff economy is the median landed-task ratio of lines changed to the fleet median for that tier and type. Above 1 means larger diffs for comparable work. Missing data or a zero reference median is unavailable. It is shown, not scored; normalizing regressions by a crew's own diff would reward bloat. All tiers keeps the tier-mix score. A selected tier ranks its crew scores from the cell matrix, pulled toward 100 with 30 tasks of reference evidence, and hides rows without work in that tier. Detail evidence remains for all tiers." })] : []),
    ...ownUnitText(),
    el("p", { class: "agents-formula", text: `score = min(raw, (${unit} × raw + ${SMOOTHING} × fleet average) ÷ (${unit} + ${SMOOTHING}))` }),
    el("p", { text: `A row above the fleet average, the mean raw score weighted by ${unit}, is pulled down toward it, the more the less work backs it; a row below keeps its raw score. So a small row never rises on the fleet's evidence, and at an equal raw score the row with more ${unit} ranks higher. Rows with fewer than ${scorecard.min_support} ${unit} are greyed, unranked and listed last. Every problem counts against one role only, and cost and time are shown, never scored.` }),
    ...(SEVERITY_ROLES.has(role) ? [el("p", { text: SEVERITY_HOW })] : []),
    ...(panel && Array.isArray(panel.how) ? panel.how.map((text) => el("p", { text })) : []),
  ]);
  details.open = howOpen;
  details.addEventListener("toggle", () => { howOpen = details.open; });
  details.dataset.key = "how";
  details.dataset.hash = JSON.stringify([role, scorecard.terms, unit, scorecard.min_support]);
  return details;
}

// Terms rated per 100 of their own unit, grouped by that unit.
function ownUnitText() {
  const byUnit = new Map();
  for (const term of terms()) {
    if (!term.unit || (role === "implementers" && REVIEW_RATE_TERMS.has(term.key))) continue;
    byUnit.set(term.unit, [...(byUnit.get(term.unit) || []), term.label]);
  }
  return [...byUnit].map(([unit, labels]) => el("p", {
    text: `${labels.join(", ")} ${labels.length === 1 ? "is" : "are"} rated per 100 ${unit} instead. On a row with too few ${unit} ${labels.length === 1 ? "it is" : "they are"} provisional: shown in italics and left out of the score.`,
  }));
}

function rolePanelNode(panel) {
  const node = el("section", { class: `agents-role-panel agents-role-panel-${role}`, "aria-label": panel.title }, [
    el("header", { class: "agents-role-panel-head" }, [
      el("h3", { text: panel.title }),
      ...(panel.hint ? [common.pinnedInfoHint(panel.hint({ scorecard }))] : []),
    ]),
    ...panel.render({ scorecard, rows: rows(), roleHref, rerender: render }),
  ]);
  node.dataset.key = "panel";
  node.dataset.hash = JSON.stringify([role, scorecard.rows, panel.hash ? panel.hash() : null]);
  return node;
}

function effortSummary(row) {
  return `Effort: ${(row.effort_breakdown || [{ reasoning_effort: "unknown" }]).map(item =>
    `${item.reasoning_effort === null ? "provider default" : item.reasoning_effort || "unknown"}${item.invocations == null ? "" : ` (${item.invocations} invocations, ${item.tasks} tasks)`}`).join("; ")}`;
}

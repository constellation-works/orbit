// The Drain Overview's account of the live window, read from its run detail
// (`GET /api/runs/:id`): how it was started, from the inputs the run recorded,
// and on a replica's pull drain the claim budget and the claims by stage that
// `orbit run show` prints from the same data.
//
// A setting the run did not record says so; nothing is filled in from the
// composer or from configuration read now.

import { el, formatDateTime, runHref } from './common.js';
import { drainCheckout, drainCommandBlock, drainCommandParts, poolEntryTexts, startDrainLike } from './drain-start.js';

const $ = (id) => document.getElementById(id);
const TIERS = [["low", "Low"], ["medium", "Medium"], ["hard", "Hard"], ["xhard", "Extra hard"]];
const NOT_RECORDED = "not recorded";

function windowText(seconds) {
  if (!Number.isFinite(seconds) || seconds <= 0) return "once";
  if (seconds % 3600 === 0) return `${seconds / 3600}h`;
  if (seconds % 60 === 0) return `${seconds / 60}m`;
  return `${seconds}s`;
}

function recordedSeconds(input) {
  if (!("for_seconds" in input)) return null;
  const raw = input.for_seconds;
  if (raw == null || raw === "") return 0;
  const seconds = Number(raw);
  return Number.isFinite(seconds) ? seconds : null;
}

// A tier this run overrode, as the values its command passed: the list the
// run recorded verbatim, else the resolved pool's entries.
function overriddenPool(input, tier, pool) {
  const raw = input[`${tier}_complexity_crews`];
  if (Array.isArray(raw)) return raw.map(String);
  if (pool && String(pool.source || "").startsWith("run_input.")) return poolEntryTexts(Array.isArray(pool.crews) ? pool.crews : []);
  return null;
}

// The run's recorded inputs as the settings shape the composer and the
// command builder read; `known` marks which ones the run recorded at all.
function recordedSettings(input, checkout) {
  const destination = input.destination && typeof input.destination === "object" ? input.destination : null;
  const source = destination ? "pull" : "local";
  const seconds = recordedSeconds(input);
  const pools = {};
  const recordedPools = input.auto_crew_pools && typeof input.auto_crew_pools === "object" ? input.auto_crew_pools : null;
  for (const [tier] of TIERS) {
    const overridden = overriddenPool(input, tier, recordedPools?.[tier]);
    if (overridden) pools[tier] = overridden;
  }
  const selector = destination?.selector || "";
  const workspace = selector.includes("/") ? selector.slice(selector.indexOf("/") + 1) : checkout.workspaceId;
  return {
    source,
    workspace: checkout.workspaceId,
    pull: workspace,
    host: destination && destination.owner_machine_id === checkout.ownerMachineId ? checkout.ownerHost : "",
    selector,
    window: seconds == null ? "" : windowText(seconds),
    concurrency: input.max_active_leaf_runs != null ? String(input.max_active_leaf_runs) : "",
    mode: input.auto_concurrency_source === "explicit" ? String(input.auto_concurrency || "") : "",
    allowCrews: Array.isArray(input.allowed_crews) ? input.allowed_crews.map(String) : [],
    pools,
    complete: input.completion === "done",
    approve: input.approve_proposed === true,
    autoFix: input.auto_fix === true,
    review: typeof input.review_request === "string" ? input.review_request : "",
    strict: input.__worker_containment_strict === true,
    claimToken: false,
  };
}

// One cell per setting: its value and the flag or config key that set it.
function startedCells(input, settings, capacity) {
  const pull = settings.source === "pull";
  const owner = pull ? "owner decides" : "";
  const cells = [];
  const cell = (label, value, from, { muted = false, missing = false } = {}) => cells.push({ label, value, from, muted, missing });
  cell("Source", pull ? `Pull from ${settings.selector}` : "Local backlog", pull ? (settings.host ? `--pull --host ${settings.host}` : "--pull") : "--workspace");
  const seconds = recordedSeconds(input);
  if (seconds == null) cell("Window", NOT_RECORDED, "--for", { missing: true });
  else {
    const until = capacity.ends_at ? ` · admits until ${formatDateTime(capacity.ends_at)}` : "";
    cell("Window", seconds > 0 ? `${windowText(seconds)}${until}` : "One pass", seconds > 0 ? "--for" : "no --for");
  }
  cell("Concurrency", settings.concurrency || "Runtime default", settings.concurrency ? "--concurrency" : "flag omitted");
  if (input.auto_concurrency) {
    const explicit = input.auto_concurrency_source === "explicit";
    cell("Admission control", String(input.auto_concurrency), explicit ? "--auto-concurrency" : "machine.auto_concurrency.mode");
  } else cell("Admission control", NOT_RECORDED, pull ? "executor host mode" : "--auto-concurrency", { missing: true });
  cell("Allowed crews", settings.allowCrews.length ? settings.allowCrews.join(", ") : "All crews", settings.allowCrews.length ? "--allow-crew" : "flag omitted");
  const recordedPools = input.auto_crew_pools && typeof input.auto_crew_pools === "object" ? input.auto_crew_pools : null;
  if (pull) cell("Complexity pools", owner, "the owner's pools", { muted: true });
  else if (!recordedPools) cell("Complexity pools", NOT_RECORDED, "--<tier>-complexity-crews", { missing: true });
  else {
    for (const [tier, label] of TIERS) {
      const pool = recordedPools[tier];
      if (!pool) {
        cell(`${label} pool`, NOT_RECORDED, `--${tier}-complexity-crews`, { missing: true });
        continue;
      }
      const crews = poolEntryTexts(Array.isArray(pool.crews) ? pool.crews : []);
      const fromRun = String(pool.source || "").startsWith("run_input.");
      cell(`${label} pool`, crews.length ? crews.join(", ") : "Off", fromRun ? `--${tier}-complexity-crews` : String(pool.source || `workflow.${tier}_complexity_crews`));
    }
  }
  if (pull) {
    cell("Authority", owner, "the owner lands", { muted: true });
    cell("Review", owner, "the owner's policy", { muted: true });
  } else {
    const authority = [settings.complete ? "marks done" : "stops at review", settings.approve ? "approves proposals" : "", settings.autoFix ? "auto-fix" : ""].filter(Boolean).join(" · ");
    const flags = [settings.complete ? "--complete" : "", settings.approve ? "--approve-proposed" : "", settings.autoFix ? "--auto-fix" : ""].filter(Boolean).join(" ");
    cell("Authority", authority, flags || "flags omitted");
    if (settings.review) cell("Review", `adds ${settings.review}`, "--review");
    else if (input.review && typeof input.review === "object" && input.review.timing) cell("Review", String(input.review.timing), "review.timing");
    else cell("Review", NOT_RECORDED, "--review", { missing: true });
  }
  cell("Worker containment", settings.strict ? "Strict" : "Default", settings.strict ? "--strict-worker-containment" : "flag omitted", { muted: pull });
  cell("Workspace claim", NOT_RECORDED, "--claim-token", { missing: true });
  return cells;
}

function startedGrid(cells) {
  const grid = el("dl", { class: "drain-started-grid" });
  for (const { label, value, from, muted, missing } of cells) {
    grid.appendChild(el("div", { class: `drain-started-cell${muted ? " owner-decides" : ""}${missing ? " not-recorded" : ""}` }, [
      el("dt", { text: label }),
      el("dd", {}, [el("span", { class: "drain-started-value", text: value }), el("code", { class: "drain-flag", text: from })]),
    ]));
  }
  return grid;
}

// N for the claim budget: the ceiling the run recorded, else the one
// readiness reports for this workspace.
function claimCeiling(input, capacity) {
  const recorded = Number(input.max_active_leaf_runs);
  if (Number.isFinite(recorded) && recorded >= 1) return Math.trunc(recorded);
  const reported = Number(capacity.max_active_leaf_runs);
  return Number.isFinite(reported) && reported >= 1 ? Math.trunc(reported) : null;
}

// `held − min(reviewing, N) < N`: a reviewing leaf frees its implementing
// slot, up to N of them, so a replica holds at most 2N claims.
function claimBudget(leaves, n) {
  const held = leaves.length;
  const reviewing = leaves.filter((leaf) => leaf.stage === "reviewing").length;
  const implementing = held - reviewing;
  const counted = held - Math.min(reviewing, n);
  const open = Math.max(0, n - counted);
  const cells = el("div", { class: "drain-budget-cells", role: "img", "aria-label": `${implementing} implementing, ${reviewing} reviewing, ${open} open of ${2 * n} claim cells` });
  const kinds = [];
  for (let i = 0; i < implementing; i += 1) kinds.push("implementing");
  for (let i = 0; i < reviewing; i += 1) kinds.push("reviewing");
  for (let i = 0; i < open; i += 1) kinds.push("open");
  while (kinds.length < 2 * n) kinds.push("later");
  for (const kind of kinds.slice(0, Math.max(2 * n, held))) cells.appendChild(el("span", { class: `drain-budget-cell ${kind}`, title: kind }));
  const legend = el("p", { class: "drain-budget-legend" }, ["implementing", "reviewing", "open", "later"].map((kind) => el("span", { class: `drain-budget-key ${kind}`, text: kind })));
  const sum = el("p", { class: "drain-budget-sum mono", id: "drain-claim-budget-sum", text: `held − min(reviewing, N) < N: ${held} − min(${reviewing}, ${n}) = ${counted} ${counted < n ? "<" : "≥"} ${n}${counted < n ? ` → claims up to ${open} more` : " → no new claim"}` });
  return el("div", { class: "drain-budget" }, [cells, legend, sum]);
}

function claimsTable(leaves) {
  const reviewing = leaves.filter((leaf) => leaf.stage === "reviewing").length;
  const head = el("p", { class: "drain-claims-head", id: "drain-claims-summary", text: leaves.length
    ? `Claimed leaves: ${leaves.length} running (${leaves.length - reviewing} implementing, ${reviewing} reviewing)`
    : "No claims held." });
  if (!leaves.length) return head;
  const rows = el("tbody");
  for (const leaf of leaves) {
    const link = el("a", { class: "mono", text: leaf.leaf_run_id || "-" });
    if (leaf.leaf_run_id) link.href = runHref(leaf.leaf_run_id);
    rows.appendChild(el("tr", {}, [
      el("td", { class: "mono", text: leaf.task_id || "?" }),
      el("td", { text: leaf.stage || "-" }),
      el("td", { text: [leaf.leaf_state, leaf.settlement_phase ? `claim ${leaf.settlement_phase}` : ""].filter(Boolean).join(" · ") || "-" }),
      el("td", {}, [link]),
    ]));
  }
  const table = el("table", { class: "drain-claims" }, [
    el("thead", {}, [el("tr", {}, ["Task", "Stage", "State", "Leaf run"].map((text) => el("th", { text })))]),
    rows,
  ]);
  return el("div", { class: "drain-claims-wrap" }, [head, el("div", { class: "drain-claims-scroll" }, [table])]);
}

function settlementsNote(detail) {
  const refused = Array.isArray(detail.refused_settlements) ? detail.refused_settlements : [];
  const breaker = detail.pull_breaker || null;
  const lines = [];
  if (breaker) lines.push(el("li", { class: "warn", text: `Claim breaker open after ${breaker.failures} failed claims; new claims wait for it to half-open.` }));
  for (const settlement of refused) {
    lines.push(el("li", { class: "warn", text: `Settlement refused by ${settlement.owner} ${settlement.refusals}×: ${settlement.reason}. ${settlement.remedy || ""}`.trim() }));
  }
  if (!lines.length) lines.push(el("li", { text: "No settlement refused; results reach the owner as leaves finish. Stop and settle retries any that wait." }));
  return el("div", { class: "drain-settlements" }, [el("h3", { class: "drain-compose-title", text: "Settlements to the owner" }), el("ul", {}, lines)]);
}

function hidePanels() {
  for (const id of ["drain-started-panel", "drain-pull-panel"]) {
    const panel = $(id);
    if (panel) panel.hidden = true;
  }
}

// Paints both panels from a run detail, or hides them with no live window.
export function renderDrainStarted(detail, payload) {
  const input = detail?.run?.input && typeof detail.run.input === "object" ? detail.run.input : null;
  if (!input) {
    hidePanels();
    return;
  }
  const checkout = { ...drainCheckout(payload), ownerMachineId: payload?.start_options?.owner_machine_id || "" };
  const settings = recordedSettings(input, checkout);
  const capacity = payload?.capacity || {};
  const started = $("drain-started-panel");
  const startedBody = $("drain-started-body");
  if (started && startedBody) {
    started.hidden = false;
    const like = el("button", { class: "operation-button", text: "Start a window like this" });
    like.type = "button";
    like.id = "drain-start-like";
    like.disabled = !settings.window;
    like.addEventListener("click", () => startDrainLike(settings));
    startedBody.replaceChildren(
      startedGrid(startedCells(input, settings, capacity)),
      drainCommandBlock(drainCommandParts(settings), { id: "drain-started-command", note: "Rebuilt from what the run recorded." }),
      el("div", { class: "drain-started-actions" }, [like]),
    );
  }
  const pullPanel = $("drain-pull-panel");
  const pullBody = $("drain-pull-body");
  if (!pullPanel || !pullBody) return;
  pullPanel.hidden = settings.source !== "pull";
  if (pullPanel.hidden) {
    pullBody.textContent = "";
    return;
  }
  const leaves = Array.isArray(detail.claimed_leaves) ? detail.claimed_leaves : [];
  const n = claimCeiling(input, capacity);
  pullBody.replaceChildren(
    el("p", { class: "drain-pull-owner" }, ["Pulling from ", el("code", { class: "mono", text: settings.selector || "the owner" }), checkout.ownerHost ? ` on ${checkout.ownerHost}` : ""]),
    el("h3", { class: "drain-compose-title", text: "Claim budget" }),
    n ? claimBudget(leaves, n) : el("p", { class: "drain-field-hint", text: "The run recorded no concurrency and readiness reported none, so the budget is unknown." }),
    el("h3", { class: "drain-compose-title", text: "Claims by stage" }),
    claimsTable(leaves),
    settlementsNote(detail),
  );
}

export function hideDrainStarted() {
  hidePanels();
}

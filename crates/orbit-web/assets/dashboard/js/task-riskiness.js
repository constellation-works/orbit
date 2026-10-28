// Task riskiness: the observed chance that a task of this size, once done,
// is named by a later regression, as the task API's `riskiness` object
// estimates it from a size-only fit over recent done tasks. The size is the
// declared path count before a diff exists, else the diff's lines changed.
// A task without an estimate says why and never reads as low risk; an
// estimate whose declared count misses pathless selectors carries a `~`.

import { el, formatDate } from './common.js';

// The short row words and the sentence for each state without a number.
const NO_ESTIMATE = {
  no_footprint: ["no files", "The task declares no context files, so there is nothing to estimate from."],
  unsupported_footprint: ["no paths", "Its context selectors name no file or directory, so there is nothing to estimate from."],
  insufficient_history: ["thin history", "Too few done tasks in the window to fit a size-based estimate."],
  unavailable: ["unavailable", "The estimate could not be computed."],
};

// An estimate at least this many times the workspace base rate is elevated.
const ELEVATED_RATIO = 1.5;

export function riskinessOf(task) {
  const riskiness = task && task.riskiness;
  return riskiness && typeof riskiness === "object" ? riskiness : null;
}

export function riskinessSignature(task) {
  const riskiness = riskinessOf(task);
  return riskiness ? JSON.stringify(riskiness) : "";
}

function estimated(riskiness) {
  return riskiness.state === "estimated" && typeof riskiness.probability === "number";
}

function percent(value) {
  if (typeof value !== "number") return "–";
  const rounded = Math.round(value * 100);
  return rounded < 1 && value > 0 ? "<1%" : `${rounded}%`;
}

function partial(riskiness) {
  return riskiness.support_state === "partial";
}

function elevated(riskiness) {
  const base = riskiness.workspace && riskiness.workspace.base_rate;
  return typeof base === "number" && base > 0 && riskiness.probability >= base * ELEVATED_RATIO;
}

function plural(n, word) {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

// The one-line value: "23%", "~23%" for a partial count, or "No estimate".
function valueText(riskiness) {
  if (!estimated(riskiness)) return "No estimate";
  return `${partial(riskiness) ? "~" : ""}${percent(riskiness.probability)}`;
}

function count(value) {
  return typeof value === "number" ? value.toLocaleString("en-US") : "–";
}

// The size as its basis counts it: "23 declared files" or "412 lines changed".
function sizeText(riskiness) {
  if (typeof riskiness.size !== "number") return null;
  if (riskiness.basis === "diff_lines") return `${count(riskiness.size)} ${riskiness.size === 1 ? "line" : "lines"} changed`;
  return `${count(riskiness.size)} declared ${riskiness.size === 1 ? "file" : "files"}`;
}

// The calibration evidence: recent done tasks at least this size.
function bandLine(riskiness) {
  const band = riskiness.size_band;
  if (!band || typeof band.tasks !== "number") return null;
  if (!band.tasks) return "No recent done task was this size or larger";
  return `Of the ${count(band.tasks)} recent done ${band.tasks === 1 ? "task" : "tasks"} this size or larger, ${count(band.regressed)} regressed (${percent(band.rate)})`;
}

const FALLBACK = {
  no_resolvable_diff: "no candidate or landed diff could be read",
  diff_history_insufficient: "too few recent done tasks have a landed diff to fit lines changed",
};

// Every line the tooltip and the detail block show, in reading order.
export function riskinessLines(riskiness) {
  if (!riskiness) return [];
  const lines = [];
  if (estimated(riskiness)) {
    const size = sizeText(riskiness);
    lines.push(`Regression risk ${percent(riskiness.probability)}${size ? ` · ${size}` : ""}${partial(riskiness) ? " (partial count)" : ""}`);
    const band = bandLine(riskiness);
    if (band) lines.push(band);
  } else {
    const [, sentence] = NO_ESTIMATE[riskiness.state] || NO_ESTIMATE.unavailable;
    lines.push(`No estimate: ${sentence}`);
  }
  if (riskiness.state === "unavailable") return lines;
  if (riskiness.basis_fallback) {
    lines.push(`Sized by declared files: ${FALLBACK[riskiness.basis_fallback] || riskiness.basis_fallback}`);
  }
  const fit = riskiness.fit;
  if (fit && typeof fit.observations === "number") {
    const population = riskiness.basis === "diff_lines" ? "with a landed diff" : "";
    lines.push(`Fitted on size alone over ${count(fit.observations)} done tasks${population ? ` ${population}` : ""}, ${count(fit.regressed)} regressed`);
  }
  const workspace = riskiness.workspace || {};
  if (riskiness.window_days != null) {
    lines.push(`Window: tasks done in the last ${riskiness.window_days} days, observed through ${formatDate(riskiness.observed_through)}; recent work has had less time to reveal regressions`);
  }
  if (workspace.tasks != null) {
    lines.push(`Workspace: ${count(workspace.regressed)} of ${count(workspace.tasks)} done tasks regressed (base ${percent(workspace.base_rate)})`);
  }
  if (riskiness.basis !== "diff_lines" && riskiness.unsupported_selectors) {
    lines.push(`Not counted: ${plural(riskiness.unsupported_selectors, "selector")} without a path`);
  }
  return lines;
}

function chipClass(riskiness) {
  if (!estimated(riskiness)) return "risk-none";
  return elevated(riskiness) ? "risk-elevated" : "risk-estimated";
}

// The Tasks row part: "risk 23%" or "risk ~23%" for a partial count.
export function buildRiskRowPart(task) {
  const riskiness = riskinessOf(task);
  if (!riskiness || !estimated(riskiness)) return null;
  const title = riskinessLines(riskiness).join("\n");
  return el("span", { class: `risk-part ${chipClass(riskiness)}`, text: `risk ${valueText(riskiness)}`, title, "aria-label": title });
}

// The task detail's lever chip: the value, with every line in its tooltip.
export function buildRiskChip(task) {
  const riskiness = riskinessOf(task);
  if (!riskiness) return null;
  const title = riskinessLines(riskiness).join("\n");
  return el("span", { class: `task-chip task-chip-risk ${chipClass(riskiness)}`, title }, [
    el("span", { text: "Risk" }),
    el("span", { class: "risk-value", text: valueText(riskiness) }),
  ]);
}

// The task detail's breakdown, readable without hover (a phone has none).
export function buildRiskDetail(task) {
  const riskiness = riskinessOf(task);
  if (!riskiness || !estimated(riskiness)) return null;
  const lines = riskinessLines(riskiness);
  const [lead, ...rest] = lines;
  const summary = el("summary", {}, [
    el("span", { class: "task-risk-label", text: "Regression risk" }),
    el("span", { class: `task-risk-value ${chipClass(riskiness)}`, text: valueText(riskiness) }),
  ]);
  const body = el("div", { class: "task-risk-body" }, [
    el("p", { class: "task-risk-lead", text: lead }),
    ...rest.map((line) => el("p", { class: "task-risk-line", text: line })),
  ]);
  return el("details", { class: "task-risk" }, [summary, body]);
}

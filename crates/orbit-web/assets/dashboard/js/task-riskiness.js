// Task riskiness: the observed chance that a task like this one, once done,
// is named by a later regression, as the task API's `riskiness` object
// estimates it from the context files' regression history. A task without an
// estimate says why and never reads as low risk; a partially supported
// estimate carries a `~` marker.

import { el, formatDate } from './common.js';

// The short row words and the sentence for each state without a number.
const NO_ESTIMATE = {
  no_footprint: ["no files", "The task declares no context files, so there is nothing to estimate from."],
  unsupported_footprint: ["no paths", "Its context selectors name no file or directory, so there is nothing to estimate from."],
  insufficient_history: ["thin history", "Too few done tasks touched its files in the window to estimate."],
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

function plural(count, word) {
  return `${count} ${word}${count === 1 ? "" : "s"}`;
}

// The one-line value: "23%", "~23%" for partial support, or "No estimate".
function valueText(riskiness) {
  if (!estimated(riskiness)) return "No estimate";
  return `${partial(riskiness) ? "~" : ""}${percent(riskiness.probability)}`;
}

function adjustmentLine(riskiness) {
  const adjustment = riskiness.adjustment;
  if (!adjustment) return null;
  const tier = riskiness.complexity || "unassessed";
  const groups = Array.isArray(adjustment.groups) ? adjustment.groups : [];
  const groupText = (group) => `${group.crew} · ${tier}: ${group.regressed} of ${group.tasks} regressed${typeof group.rate === "number" ? ` (${percent(group.rate)})` : ""}`;
  if (adjustment.state === "applied") return `Adjusted for ${groupText(groups[0] || { crew: riskiness.crew, regressed: 0, tasks: 0 })}`;
  if (adjustment.state === "pool_weighted") {
    const unsupported = groups.filter((group) => !group.applied).map((group) => group.crew);
    return `Averaged over the ${tier} crew pool by weight${unsupported.length ? `; unadjusted for ${unsupported.join(", ")} (below ${adjustment.min_support} tasks)` : ""}`;
  }
  const min = adjustment.min_support;
  const reasons = {
    no_complexity: "the task has no assessed complexity",
    no_crew: "the task has no crew and its complexity has no crew pool",
    workspace_below_support: `the workspace has fewer than ${min} done tasks`,
    group_below_support: groups[0] ? `${groupText(groups[0])}, below ${min} tasks` : `its crew group is below ${min} tasks`,
    pool_below_support: `no ${tier} pool crew has ${min} done tasks`,
  };
  return `Not adjusted for crew/complexity: ${reasons[adjustment.reason] || adjustment.reason || "no basis"}`;
}

// Every line the tooltip and the detail block show, in reading order.
export function riskinessLines(riskiness) {
  if (!riskiness) return [];
  const lines = [];
  if (estimated(riskiness)) {
    lines.push(`Regression risk ${percent(riskiness.probability)}${partial(riskiness) ? " (partial support)" : ""}: the observed share of similar done tasks later named by a regression`);
  } else {
    const [, sentence] = NO_ESTIMATE[riskiness.state] || NO_ESTIMATE.unavailable;
    lines.push(`No estimate: ${sentence}`);
  }
  if (riskiness.state === "unavailable") return lines;
  const workspace = riskiness.workspace || {};
  if (riskiness.window_days != null) {
    lines.push(`Window: tasks done in the last ${riskiness.window_days} days, observed through ${formatDate(riskiness.observed_through)}; recent work has had less time to reveal regressions`);
  }
  if (workspace.tasks != null) {
    lines.push(`Workspace: ${workspace.regressed} of ${workspace.tasks} done tasks regressed (base ${percent(workspace.base_rate)})`);
  }
  const files = Array.isArray(riskiness.driving_files) ? riskiness.driving_files : [];
  if (files.length) {
    lines.push("Driving files:");
    for (const file of files) lines.push(`  ${file.path} — ${percent(file.probability)} (${file.regressed} of ${file.support} regressed)`);
  }
  if (riskiness.supporting_tasks) {
    lines.push(`Support: ${plural(riskiness.supporting_tasks, "distinct done task")}, ${riskiness.supporting_regressed} regressed`);
  }
  const gaps = [];
  if (riskiness.uncovered_anchors) gaps.push(`${riskiness.uncovered_anchors} of ${riskiness.anchors} paths below ${riskiness.min_support} done tasks`);
  if (riskiness.unsupported_selectors) gaps.push(`${plural(riskiness.unsupported_selectors, "selector")} without a path`);
  if (gaps.length) lines.push(`Uncovered: ${gaps.join("; ")}`);
  const adjustment = adjustmentLine(riskiness);
  if (adjustment) lines.push(adjustment);
  return lines;
}

function chipClass(riskiness) {
  if (!estimated(riskiness)) return "risk-none";
  return elevated(riskiness) ? "risk-elevated" : "risk-estimated";
}

// The Tasks row part: "risk 23%" or "risk ~23%" when support is partial.
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
    ...rest.map((line) => el("p", {
      class: line.startsWith("  ") ? "task-risk-file mono" : "task-risk-line",
      text: line.trim(),
    })),
  ]);
  return el("details", { class: "task-risk" }, [summary, body]);
}

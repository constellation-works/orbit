// Observe › Agents › Orchestrators: the role panel, where each orchestrator's
// rework went by cause. Only spec-caused rework counts against an
// orchestrator; crew-caused rework counts against Implementers, and
// infrastructure and no-cause rework are not scored. The counts are the
// rows' shown-not-scored `rework_*` stats. Rework and regressions with no
// recorded cause (the `unclassified` stat) are shown beside each score as its
// confidence. Every node is built with `el()`.

import { el } from './common.js';

const CAUSES = [
  { key: "rework_spec", label: "Spec", note: "counted here" },
  { key: "rework_crew", label: "Crew", note: "counted against Implementers", role: "implementers" },
  { key: "rework_infrastructure", label: "Infrastructure", note: "not scored" },
  { key: "rework_no_cause", label: "No cause", note: "not scored, shown beside the score" },
];

// What "How the score works" adds for orchestrators.
const HOW = [
  "A landed task earns one point whatever its complexity: complexity is assessed when a task is filed or piloted, so weighting by it would let an orchestrator raise its own credit by rating its tasks harder. A spec defect or spec-caused rework costs as much as a landing, so a task whose spec needed fixing earns nothing.",
  "Rework and regressions with no recorded cause count against no one. Their count shows beside a score, because a row with many of them is scored on less evidence than it appears.",
];

function causeCount(row, key) {
  const stat = (Array.isArray(row.stats) ? row.stats : []).find((entry) => entry.key === key);
  return stat && typeof stat.value === "number" ? stat.value : 0;
}

function scoreNote(row) {
  const count = causeCount(row, "unclassified");
  if (!count) return null;
  const rate = row.units ? Math.round((count * 1000) / row.units) / 10 : 0;
  return {
    text: `${count} unclassified`,
    detail: `${count} of the rework and regressions on these ${row.units} owned tasks have no recorded cause (${rate} per 100). They are not scored, so this score rests on less evidence than it shows.`,
  };
}

function render({ rows, roleHref }) {
  const totals = rows.map((row) => CAUSES.reduce((sum, cause) => sum + causeCount(row, cause.key), 0));
  const largest = Math.max(1, ...totals);
  const legend = el("ul", { class: "agents-cause-legend" }, CAUSES.map((cause) => {
    const note = cause.role
      ? el("a", { class: "agents-cause-note", text: cause.note })
      : el("span", { class: "agents-cause-note", text: cause.note });
    if (cause.role) note.href = roleHref(cause.role);
    return el("li", {}, [
      el("span", { class: `agents-swatch agents-cause-${cause.key}`, "aria-hidden": "true" }),
      el("span", { text: cause.label }),
      note,
    ]);
  }));
  const list = el("ul", { class: "agents-cause-rows" });
  rows.forEach((row, index) => {
    const total = totals[index];
    const bar = el("span", { class: "agents-cause-bar", role: "img" });
    bar.setAttribute("aria-label", CAUSES.map((cause) => `${cause.label} ${causeCount(row, cause.key)}`).join(", "));
    for (const cause of CAUSES) {
      const count = causeCount(row, cause.key);
      if (!count) continue;
      bar.append(el("span", {
        class: `agents-cause-seg agents-cause-${cause.key}`,
        title: `${cause.label}: ${count} (${cause.note})`,
        style: { width: `${(count * 100) / largest}%` },
      }));
    }
    const item = el("li", { class: row.below_support ? "agents-cause-row agents-row-below" : "agents-cause-row" }, [
      el("span", { class: "agents-cause-name", text: row.crew }),
      bar,
      el("span", { class: "agents-cause-total", text: total ? `${total} rework` : "no rework" }),
    ]);
    item.dataset.row = `${row.crew}\0${row.model}`;
    list.append(item);
  });
  return [
    el("p", { class: "agents-muted", text: "Repeat runs, [BLOCKED] or unmerged PRs and operator requeues on each orchestrator's tasks, by cause. Only spec-caused rework lowers an orchestrator's score." }),
    legend,
    list,
  ];
}

export const orchestratorsPanel = { title: "Rework by cause", render, scoreNote, how: HOW };

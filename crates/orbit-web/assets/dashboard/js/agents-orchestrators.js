// Observe › Agents › Orchestrators: the role panel, where each orchestrator's
// rework went by cause. Only spec-caused rework counts against an
// orchestrator; crew-caused rework counts against Implementers, and
// infrastructure and no-cause rework are not scored. The counts are the
// rows' shown-not-scored `rework_*` stats. Every node is built with `el()`.

import { el } from './common.js';

const CAUSES = [
  { key: "rework_spec", label: "Spec", note: "counted here" },
  { key: "rework_crew", label: "Crew", note: "counted against Implementers", role: "implementers" },
  { key: "rework_infrastructure", label: "Infrastructure", note: "not scored" },
  { key: "rework_no_cause", label: "No cause", note: "not scored" },
];

function causeCount(row, key) {
  const stat = (Array.isArray(row.stats) ? row.stats : []).find((entry) => entry.key === key);
  return stat && typeof stat.value === "number" ? stat.value : 0;
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

export const orchestratorsPanel = { title: "Rework by cause", render };

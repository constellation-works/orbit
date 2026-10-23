// Observe › Agents › Reviewers: the role panel, how each reviewer's settled
// reviews turned out, plus the provisional banner and the `accepts often`
// row tag. The outcomes are the rows' shown-not-scored `outcome_*` stats; the
// tag reads the `accepts_often` stat, an accept rate 10 or more points above
// the other reviewers on the same tiers. Neither enters the score. Every node
// is built with `el()`.

import { el } from './common.js';

const OUTCOMES = [
  { key: "outcome_held_up", label: "Accepted and held up" },
  { key: "outcome_caught", label: "Caught a real problem" },
  { key: "outcome_false_alarm", label: "False alarm" },
  { key: "outcome_missed", label: "Missed a regression" },
  { key: "outcome_unresolved", label: "Unresolved", note: "not judged yet, incomplete or failed fixes" },
];
// Reviews younger than this can still be charged a `review_missed`
// regression (`roles/reviewers.rs`).
const MISS_LOOKBACK_DAYS = 30;

function statValue(row, key) {
  const stat = (Array.isArray(row.stats) ? row.stats : []).find((entry) => entry.key === key);
  return stat ? stat.value : null;
}

function outcomeCount(row, key) {
  const value = statValue(row, key);
  return typeof value === "number" ? value : 0;
}

function plural(n, one, many = `${one}s`) {
  return `${n} ${n === 1 ? one : many}`;
}

function render({ rows }) {
  const totals = rows.map((row) => OUTCOMES.reduce((sum, outcome) => sum + outcomeCount(row, outcome.key), 0));
  const largest = Math.max(1, ...totals);
  const legend = el("ul", { class: "agents-cause-legend" }, OUTCOMES.map((outcome) => el("li", {}, [
    el("span", { class: `agents-swatch agents-outcome-${outcome.key}`, "aria-hidden": "true" }),
    el("span", { text: outcome.label }),
    ...(outcome.note ? [el("span", { class: "agents-cause-note", text: outcome.note })] : []),
  ])));
  const list = el("ul", { class: "agents-cause-rows" });
  rows.forEach((row, index) => {
    const total = totals[index];
    const bar = el("span", { class: "agents-cause-bar", role: "img" });
    bar.setAttribute("aria-label", OUTCOMES.map((outcome) => `${outcome.label} ${outcomeCount(row, outcome.key)}`).join(", "));
    for (const outcome of OUTCOMES) {
      const count = outcomeCount(row, outcome.key);
      if (!count) continue;
      const segment = el("span", {
        class: `agents-cause-seg agents-outcome-${outcome.key}`,
        title: `${outcome.label}: ${count}`,
        style: { width: `${(count * 100) / largest}%` },
      });
      segment.dataset.outcome = outcome.key;
      bar.append(segment);
    }
    const item = el("li", { class: row.below_support ? "agents-cause-row agents-row-below" : "agents-cause-row" }, [
      el("span", { class: "agents-cause-name", text: row.crew, title: `${row.crew} · ${row.model}` }),
      bar,
      el("span", { class: "agents-cause-total", text: plural(total, "review") }),
    ]);
    item.dataset.row = `${row.crew}\0${row.model}`;
    list.append(item);
  });
  return [
    el("p", { class: "agents-muted", text: "Each settled review by how its verdict turned out. Accepting or rejecting more never moves a score; only verdicts that turned out right earn credit." }),
    legend,
    list,
  ];
}

// While any review in the window is younger than the miss lookback, a
// regression filed later can still lower the reviewer who accepted.
function banner({ scorecard }) {
  const provisional = scorecard && scorecard.provisional;
  if (typeof provisional !== "number" || provisional <= 0) return null;
  const units = (Array.isArray(scorecard.rows) ? scorecard.rows : []).reduce((sum, row) => sum + (row.units || 0), 0);
  return el("p", {}, [
    el("strong", { text: "Provisional. " }),
    el("span", {
      text: `${provisional} of ${plural(units, "review")} in this window ${provisional === 1 ? "is" : "are"} younger than the ${MISS_LOOKBACK_DAYS}-day miss lookback.`
        + " A review_missed regression filed later still lowers the reviewer who accepted, so these scores can fall.",
    }),
  ]);
}

function rowTags(row) {
  if (statValue(row, "accepts_often") !== true) return [];
  const rate = statValue(row, "accept_rate");
  const peers = statValue(row, "peer_accept_rate");
  const tag = el("span", {
    class: "agents-tag agents-tag-accepts-often",
    text: "accepts often",
    title: `Accepts ${rate}% of reviews; the other reviewers accept ${peers}% on the same tiers. Shown, not scored.`,
  });
  tag.dataset.tag = "accepts_often";
  return [tag];
}

export const reviewersPanel = { title: "How verdicts turned out", render, banner, rowTags };

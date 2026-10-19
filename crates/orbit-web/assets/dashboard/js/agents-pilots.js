// Observe › Agents › Pilots: the role panel, how well each pilot's assessed
// tiers ordered the work. The observed tier ranks the window's assessed tasks
// by total implementation cost and cuts the ranking in the proportions the
// pilots assigned, so each row's matrix compares the tier a task was
// assessed at with the tier its cost placed it in. The diagonal is the
// calibration term; above it the pilot assessed too easy, below it too hard.
// The counts are the rows' shown-not-scored `tier_matrix` stat. Every node
// is built with `el()`.

import { el } from './common.js';

const TIER_LABELS = { low: "Low", medium: "Medium", hard: "Hard", xhard: "X-hard" };

function matrixOf(row) {
  const stat = (Array.isArray(row.stats) ? row.stats : []).find((entry) => entry.key === "tier_matrix");
  const value = stat && stat.value;
  return value && Array.isArray(value.tiers) && Array.isArray(value.rows) ? value : null;
}

const count = (value) => (typeof value === "number" && Number.isFinite(value) ? value : 0);
const label = (tier) => TIER_LABELS[tier] || tier;

function matrixTable(row, matrix) {
  const caption = `${row.crew} · ${row.model}: assessed tier by observed tier`;
  const head = el("tr", {}, [
    el("th", { scope: "col", text: "Assessed ↓ · observed →" }),
    ...matrix.tiers.map((tier) => el("th", { scope: "col", class: "agents-num", text: label(tier) })),
  ]);
  const body = matrix.rows.map((line, assessed) => {
    const tr = el("tr", {}, [
      el("th", { scope: "row", text: label(line.assessed) }),
      ...matrix.tiers.map((tier, observed) => {
        const value = count(line.observed && line.observed[observed]);
        const place = assessed === observed ? "on" : assessed < observed ? "easy" : "hard";
        const cell = el("td", { class: `agents-num agents-tier-cell is-${place}${value ? "" : " is-zero"}`, text: String(value) });
        cell.dataset.assessed = line.assessed;
        cell.dataset.observed = tier;
        return cell;
      }),
    ]);
    tr.dataset.tier = line.assessed;
    return tr;
  });
  return el("table", { class: "agents-table agents-tier-matrix" }, [
    el("caption", { class: "visually-hidden", text: caption }),
    el("thead", {}, [head]),
    el("tbody", {}, body),
  ]);
}

// A wide matrix scrolls inside its own focusable frame at narrow widths.
function scroller(row, matrix) {
  const wrap = el("div", { class: "agents-table-wrap", role: "region", "aria-label": `${row.crew} · ${row.model} tier matrix` }, [matrixTable(row, matrix)]);
  wrap.tabIndex = 0;
  return wrap;
}

function card(row, matrix) {
  const placed = count(matrix.placed);
  const summary = el("dl", { class: "agents-tier-counts" }, [
    ["On tier", count(matrix.on_tier), "on"],
    ["Too easy", count(matrix.too_easy), "easy"],
    ["Too hard", count(matrix.too_hard), "hard"],
    ["Not placed", count(matrix.not_placed), "unplaced"],
  ].flatMap(([text, value, key]) => {
    const dd = el("dd", { text: String(value) });
    dd.dataset.count = key;
    return [el("dt", { text }), dd];
  }));
  const node = el("article", { class: `agents-tier-card${row.below_support ? " agents-row-below" : ""}` }, [
    el("h4", {}, [
      el("span", { class: "agents-tier-crew", text: row.crew }),
      el("span", { class: "agents-model", text: row.model }),
    ]),
    placed
      ? scroller(row, matrix)
      : el("p", { class: "agents-muted", text: "No assessed task with a tier and a priced implementation." }),
    summary,
  ]);
  node.dataset.row = `${row.crew}\0${row.model}`;
  return node;
}

function render({ rows }) {
  const cards = rows.map((row) => [row, matrixOf(row)]).filter(([, matrix]) => matrix);
  return [
    el("p", {
      class: "agents-muted",
      text: "Rows are the tier each task was assessed at; columns are the tier its total implementation cost placed it in, after cutting the window's cost ranking in the proportions the pilots assigned. The diagonal earns tier calibration. Too easy: assessed below the observed tier. Too hard: assessed above it. Tasks without a tier or a priced implementation are not placed.",
    }),
    cards.length
      ? el("div", { class: "agents-tier-cards" }, cards.map(([row, matrix]) => card(row, matrix)))
      : el("p", { class: "agents-cost-empty", text: "No row has an assessed tier in this window." }),
  ];
}

export const pilotsPanel = { title: "Tier calibration", render };

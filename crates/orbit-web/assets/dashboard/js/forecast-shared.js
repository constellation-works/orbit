// What the Analytics › Forecast tabs share: the selected view with its payload,
// the labels several tabs read, and the card, table, collapsed-section and
// info-icon builders.

import { el, pinnedInfoHint, detailsPanel } from './common.js';

export { el, pinnedInfoHint };

// `render` is set by forecast.js, which owns fetching and the page.
export const forecastView = {
  history: "default",
  scenario: "current",
  tab: "forecast",
  payload: null,
  historyPage: 0,
  residualMetric: "hours",
  render: () => {},
};

export const COMPLEXITY_LABELS = {
  low: "Low", medium: "Medium", hard: "Hard", xhard: "Xhard", unassessed: "Unassessed",
};
const REVIEW_COVERAGE_KEYS = ["review_mode_fallbacks", "review_mode_unknown", "history_review_mode_unknown"];

// Review-mode coverage is present on current forecasts and absent on older
// ones. Per-task `review_mode` is the same contract, so either is enough.
export function reportsReviewMode(payload = forecastView.payload) {
  const coverage = payload?.coverage;
  if (coverage && REVIEW_COVERAGE_KEYS.some((key) => Object.prototype.hasOwnProperty.call(coverage, key))) {
    return true;
  }
  return (payload?.forecast?.task_predictions || []).some((row) => row && (row.review_mode != null || row.review_mode_fallback != null));
}

export function scheduleShowsReview(predictions) {
  return predictions.some((row) => row && (row.review_mode != null || row.review_mode_fallback != null));
}

export function forecastCard(title, meta, children, className = "") {
  return el("section", { class: `mx-card ${className}`.trim(), "aria-label": title }, [
    el("div", { class: "mx-card-head" }, [
      el("h2", { class: "mx-card-title", text: title }),
      // A string is a meta line; a node (such as an info hint) goes in as is.
      typeof meta === "string" ? el("span", { class: "mx-card-meta", text: meta }) : meta || null,
    ]),
    ...children,
  ]);
}

export function forecastTable(label, head, rows, caption) {
  return el("div", { class: "mx-table-wrap", tabindex: "0", role: "region", "aria-label": label }, [
    el("table", { class: "mx-table fc-table" }, [
      caption ? el("caption", { text: caption }) : null,
      el("thead", {}, [el("tr", {}, head.map(([text, numeric]) => el("th", { scope: "col", class: numeric ? "num" : null, text })))]),
      el("tbody", {}, rows),
    ]),
  ]);
}

export function forecastMoreSection(key, title, meta, children) {
  const panel = detailsPanel(`forecast:${key}`, { class: "fc-more" });
  panel.append(el("summary", { class: "fc-more-summary" }, [
    el("span", { class: "fc-more-title", text: title }),
    meta ? el("span", { class: "fc-more-meta", text: meta }) : null,
  ]));
  panel.append(el("div", { class: "fc-more-body" }, children));
  return panel;
}

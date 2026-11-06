// What the Analytics › Forecast tabs share: the selected view with its payload,
// the labels several tabs read, and the card, table, collapsed-section and
// info-icon builders.

import { el as makeEl, infoHint, detailsPanel } from './common.js';

// `el` from common.js, also applying the table and focus attributes it leaves out.
export function el(tag, opts = {}, children = []) {
  const node = makeEl(tag, opts, children);
  for (const name of ["scope", "tabindex", "colspan"]) {
    if (opts[name] != null) node.setAttribute(name, String(opts[name]));
  }
  return node;
}

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
      meta ? el("span", { class: "mx-card-meta", text: meta }) : null,
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

// The bubble is fixed to the viewport, so a table's own scroll box cannot clip
// it, and it flips above the icon when it would run off the bottom. CSS opens
// it on hover and focus; these only say where it goes.
let followsScroll = false;

function placeForecastHint(hint) {
  if (!followsScroll) {
    followsScroll = true;
    document.addEventListener("scroll", () => {
      const open = document.querySelector(".fc-info:hover, .fc-info:focus");
      if (open) placeForecastHint(open);
    }, { capture: true, passive: true });
  }
  const bubble = hint.querySelector(".info-hint-text");
  if (!bubble) return;
  const gutter = 12;
  const width = Math.min(320, window.innerWidth - gutter * 2);
  const icon = hint.getBoundingClientRect();
  const left = Math.max(gutter, Math.min(icon.left + icon.width / 2 - width / 2, window.innerWidth - width - gutter));
  hint.style.setProperty("--fc-tip-left", `${left}px`);
  hint.style.setProperty("--fc-tip-width", `${width}px`);
  const height = bubble.getBoundingClientRect().height;
  const below = icon.bottom - 10;
  const top = below + height + gutter > window.innerHeight ? Math.max(gutter, icon.top + 10 - height) : below;
  hint.style.setProperty("--fc-tip-top", `${top}px`);
}

// infoHint with a 44px target that also opens on tap, even in a browser that
// does not focus a tapped span, and closes on Escape. Lines in `text` stay
// separate lines in the bubble.
export function forecastHint(text) {
  const hint = infoHint(text);
  hint.classList.add("fc-info");
  const place = () => placeForecastHint(hint);
  hint.addEventListener("mouseenter", place);
  hint.addEventListener("focus", place);
  hint.addEventListener("click", (event) => {
    event.preventDefault();
    event.stopPropagation();
    hint.focus();
    place();
  });
  hint.addEventListener("keydown", (event) => {
    if (event.key === "Escape") hint.blur();
  });
  return hint;
}

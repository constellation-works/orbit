// Analytics › Metrics "Lines changed" trend (ORB-18383): code and test lines
// added and deleted by landed deliveries in each bucket, as four line kinds
// or one line per implementing crew.
//
// The complexity filter is the endpoint's and refetches only this trend;
// grouping, statistic (totals or per-task mean/median) and the crew measure
// are local and redraw from the cached payload. Crews draw from complexity
// pools, so the crew view is meant to be read one complexity at a time. The
// window comes from the shared selector; chart drawing and tooltips stay in
// metrics.js, which passes its helpers in.

import { el, fetchJson, getWorkspaceRevision } from './common.js';

const COMPLEXITIES = [["", "All complexities"], ["low", "Low"], ["medium", "Medium"], ["hard", "Hard"], ["xhard", "Xhard"], ["unassessed", "Unassessed"]];
const GROUPS = [["lines", "Line kind"], ["crew", "Crew"]];
const STATS = [["sum", "Totals"], ["mean", "Mean"], ["median", "Median"]];
const KINDS = [
  { key: "code_added", label: "Code added", dashed: false },
  { key: "code_deleted", label: "Code deleted", dashed: true },
  { key: "tests_added", label: "Tests added", dashed: false },
  { key: "tests_deleted", label: "Tests deleted", dashed: true },
];
const MEASURES = [["code_added", "Code added"], ["code_deleted", "Code deleted"], ["tests_added", "Tests added"], ["tests_deleted", "Tests deleted"], ["net", "Net lines"]];
const STAT_WORDS = { sum: "total", mean: "mean per task", median: "median per task" };

const filters = { complexity: "" };
/// Local view state; metrics.js keys series visibility by `group`.
export const diffView = { group: "lines", stat: "sum", measure: "code_added" };
let sequence = 0;

/// The endpoint query for `windowKey` under the current complexity filter.
export function diffQuery(windowKey) {
  const query = new URLSearchParams({ since: windowKey });
  if (filters.complexity) query.set("complexity", filters.complexity);
  return `/api/diagnostics/diff_stats?${query}`;
}

/// Fetch the trend for `windowKey`. A failed read, or a host whose Orbit
/// predates the endpoint, answers `{ unavailable }` so the other trends still
/// render. Every result carries a fence (generation, visit revision, window
/// and query) captured at request time, as the tokens trend does.
export function fetchDiff(windowKey) {
  const fence = { generation: ++sequence, revision: getWorkspaceRevision(), windowKey, query: diffQuery(windowKey) };
  const query = fence.query;
  return fetchJson(query).then(
    (payload) => (payload && payload.window && payload.overall
      ? { ...payload, query, diffFence: fence }
      : { query, unavailable: "This host answered without a lines-changed trend.", diffFence: fence }),
    (error) => ({
      query,
      unavailable: error.status === 404 ? "This host's Orbit does not record lines changed yet. Upgrade it to see this trend." : error.message,
      diffFence: fence,
    }),
  );
}

/// True only when `payload` is the newest diff request and its visit, window
/// and query still match current state.
export function isCurrentDiffPayload(payload, windowKey) {
  const fence = payload && payload.diffFence;
  if (!fence) return false;
  return fence.generation === sequence
    && fence.revision === getWorkspaceRevision()
    && fence.windowKey === windowKey
    && fence.query === diffQuery(windowKey);
}

/// "12.4k", "860", "−1.2k".
export function fmtLines(value) {
  if (value == null || !Number.isFinite(Number(value))) return "—";
  const n = Number(value);
  const sign = n < 0 ? "−" : "";
  const abs = Math.abs(n);
  if (abs >= 1e6) return `${sign}${(abs / 1e6).toFixed(abs >= 1e7 ? 1 : 2)}M`;
  if (abs >= 1e4) return `${sign}${Math.round(abs / 1e3)}k`;
  if (abs >= 1e3) return `${sign}${(abs / 1e3).toFixed(1)}k`;
  return `${sign}${Number.isInteger(abs) ? abs : abs.toFixed(1)}`;
}

function deliveries(n) {
  return `${n} ${n === 1 ? "delivery" : "deliveries"}`;
}

/// A 1–2–5 axis covering `[min, max]` and zero, in about four steps.
function linesScale(min, max) {
  const low = Math.min(0, min);
  const high = Math.max(1, max);
  const raw = (high - low) * 1.08 / 4;
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 5, 10].map((m) => m * magnitude).find((candidate) => candidate >= raw);
  return { step, min: low < 0 ? Math.floor(low * 1.08 / step) * step : 0, max: Math.ceil(high * 1.08 / step) * step };
}

/// The filter row: complexity (refetches), grouping, statistic and, for the
/// crew view, the measure.
export function diffTools({ segGroup, windowKey, rerender, onChange }) {
  const complexity = el("select", { class: "mx-select", "aria-label": "Task complexity" },
    COMPLEXITIES.map(([value, text]) => option(value, text, value === filters.complexity)));
  complexity.addEventListener("change", () => {
    filters.complexity = complexity.value;
    // Clear to loading at once so the chart never shows the previous filter.
    onChange(null);
    fetchDiff(windowKey).then((payload) => {
      if (isCurrentDiffPayload(payload, windowKey)) onChange(payload);
    });
  });
  const local = (update) => {
    Object.assign(diffView, update);
    rerender();
  };
  const measure = el("select", { class: "mx-select", "aria-label": "Crew measure" },
    MEASURES.map(([value, text]) => option(value, text, value === diffView.measure)));
  measure.addEventListener("change", () => local({ measure: measure.value }));
  return [
    complexity,
    segGroup("Group by", GROUPS, diffView.group, (key) => key !== diffView.group && local({ group: key })),
    segGroup("Statistic", STATS, diffView.stat, (key) => key !== diffView.stat && local({ stat: key })),
    diffView.group === "crew" ? measure : null,
  ];
}

function option(value, text, selected) {
  const node = el("option", { text });
  node.value = value;
  node.selected = selected;
  return node;
}

/// The series one grouping draws: `{ key, swatch, label, dashed, measure, data }`.
function seriesOf(payload) {
  if (diffView.group === "lines") {
    return KINDS.map((kind) => ({ key: kind.key, swatch: kind.key, label: kind.label, dashed: kind.dashed, measure: kind.key, data: payload.overall }));
  }
  return (payload.crews || []).map((crew, index) => ({
    key: crew.key === "other" ? "other-crews" : `crew-${crew.key}`,
    swatch: crew.key === "other" ? "crew-other" : `c${index + 1}`,
    label: crew.key === "other" ? `Other crews (${crew.crews.length})` : crew.key,
    dashed: false,
    measure: diffView.measure,
    data: crew,
  }));
}

/// The value a statistic reads from one cell for `measure`.
function statOf(cell, measure) {
  return cell && cell.n ? cell[diffView.stat][measure] : null;
}

/// The trend model metrics.js draws: cards, chart, table, or an empty state.
export function diffModel(payload, { windowKey, shown, bucketTitle, bucketWidthLabel }) {
  const blurb = "Lines added and deleted by deliveries that landed in each bucket, split into code and tests by the workspace's test-path setting. Pick one complexity to compare crews like for like.";
  if (!payload) return { blurb, cards: [], empty: { title: "Loading lines changed…", detail: "", noWiden: true } };
  if (payload.unavailable) return { blurb, cards: [], empty: { title: "Lines changed unavailable", detail: payload.unavailable, noWiden: true } };

  const seconds = payload.window.bucket_seconds;
  const starts = payload.bucket_starts || [];
  const minDeliveries = payload.min_deliveries;
  const total = payload.overall.total;
  const series = seriesOf(payload);
  const isShown = (key) => shown[key] !== false;
  const statWord = STAT_WORDS[diffView.stat];
  const measureLabel = (MEASURES.find(([key]) => key === diffView.measure) || [null, diffView.measure])[1];

  const cards = series.map((entry) => {
    const cell = entry.data.total;
    return {
      key: entry.key,
      swatch: entry.swatch,
      dashed: entry.dashed,
      label: entry.label,
      tag: "",
      on: isShown(entry.key),
      value: fmtLines(statOf(cell, entry.measure)),
      sub: cell.n ? (diffView.stat === "sum" ? `median ${fmtLines(cell.median[entry.measure])} per task` : `total ${fmtLines(cell.sum[entry.measure])}`) : "no deliveries",
      foot: `${deliveries(cell.n)}${diffView.group === "crew" && entry.measure !== "net" && cell.n ? ` · net ${fmtLines(cell.sum.net)}` : ""}`,
    };
  });

  const scope = filters.complexity ? `${COMPLEXITIES.find(([key]) => key === filters.complexity)[1].toLowerCase()} ` : "";
  if (!total.n) {
    return {
      blurb,
      cards,
      empty: {
        title: `No ${scope}landed deliveries with lines changed in the last ${windowKey === "all" ? "recorded history" : windowKey}`,
        detail: "A delivery records its lines when it lands. To fill deliveries that landed earlier, run orbit migrate --backfill-diff-stats on the owner.",
      },
    };
  }

  const visible = series.filter((entry) => isShown(entry.key));
  let min = 0;
  let max = 0;
  for (const entry of visible) {
    for (const point of entry.data.points) {
      const value = statOf(point, entry.measure);
      if (value == null) continue;
      min = Math.min(min, value);
      max = Math.max(max, value);
    }
  }
  const scale = linesScale(min, max);
  const span = scale.max - scale.min;
  const y = (value) => 100 - (Math.min(Math.max(value, scale.min), scale.max) - scale.min) / span * 100;
  const yTicks = [];
  for (let v = scale.min; v <= scale.max; v += scale.step) yTicks.push({ top: y(v), label: fmtLines(v) });

  const notes = [
    `${bucketWidthLabel(seconds)}, aligned to the clock`,
    "time = when the delivery landed",
    diffView.stat === "sum" ? "totals of every delivery in the bucket" : `${statWord} of the deliveries in the bucket`,
    "net = code and tests added minus deleted",
  ];
  if (diffView.group === "crew") notes.push(`measure: ${measureLabel.toLowerCase()}`);
  if (diffView.group === "crew" && !filters.complexity) notes.push("all complexities mixed: crews draw from complexity pools");

  return {
    blurb,
    cards,
    chart: {
      kind: "diff",
      buckets: starts.map((start) => ({ start })),
      seconds,
      yTicks,
      lines: visible.map((entry) => ({
        key: entry.swatch,
        cls: entry.dashed ? "mx-dashed" : "",
        joined: true,
        points: entry.data.points.map((point, i) => {
          const value = statOf(point, entry.measure);
          return value == null ? null : { i, y: y(value), hollow: point.thin };
        }),
      })),
      notes,
      hollowText: `hollow = fewer than ${minDeliveries} deliveries`,
      aria: `Lines changed per landed delivery, ${statWord}, window ${windowKey}: ${visible.map((entry) => `${entry.label} ${fmtLines(statOf(entry.data.total, entry.measure))}`).join(", ")} over ${deliveries(total.n)}. Use the Table view for per-bucket values.`,
      tip: (index) => {
        const point = payload.overall.points[index];
        const withMedian = diffView.stat !== "median";
        const tip = { title: bucketTitle(starts[index], seconds, index === starts.length - 1), rows: [], foot: `${diffView.stat === "sum" ? "Total lines" : `Lines, ${statWord}`}${withMedian ? " · med = median per task" : ""} · ×n = deliveries` };
        for (const entry of series) {
          const cell = entry.data.points[index];
          if (!cell.n) continue;
          tip.rows.push({
            key: entry.swatch,
            dashed: entry.dashed,
            name: entry.label,
            n: `×${cell.n}`,
            low: cell.thin,
            value: `${fmtLines(statOf(cell, entry.measure))}${withMedian ? ` · med ${fmtLines(cell.median[entry.measure])}` : ""}${diffView.group === "crew" && entry.measure !== "net" ? ` · net ${fmtLines(cell.sum.net)}` : ""}`,
          });
        }
        if (!tip.rows.length) tip.empty = "No delivery landed in this bucket.";
        else tip.extra = `${deliveries(point.n)} · net ${fmtLines(point.sum.net)} lines · +${fmtLines(point.sum.code_added + point.sum.tests_added)} / −${fmtLines(point.sum.code_deleted + point.sum.tests_deleted)}`;
        if (tip.rows.some((row) => row.low)) tip.warn = `Amber counts: fewer than ${minDeliveries} deliveries.`;
        return tip;
      },
    },
    table: {
      caption: `Lines changed, ${statWord}, deliveries in parentheses · ${bucketWidthLabel(seconds)} · newest first`,
      heads: series.map((entry) => entry.label),
      rows: starts.map((start, i) => ({
        title: bucketTitle(start, seconds, i === starts.length - 1),
        cells: series.map((entry) => {
          const cell = entry.data.points[i];
          return cell.n ? `${fmtLines(statOf(cell, entry.measure))} (${cell.n})` : null;
        }),
      })).reverse(),
    },
  };
}

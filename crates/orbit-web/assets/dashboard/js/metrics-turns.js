// Analytics › Metrics "Turns to first edit" trend (ORB-18542): tool calls
// and the result bytes returned before the first file edit of each task's
// first implement invocation, one line per complexity, optionally split
// into with-tag and without-tag cohorts.
//
// The trend owns its filters (outcome, provider and tags) and refetches
// only its own endpoint when they change. Turns versus result bytes is
// local and redraws from the cached payload. The window comes from the
// shared selector. Chart drawing and tooltips stay in metrics.js.

import { el } from './common.js';
import { OUTCOMES, TERMINAL_TIME_NOTE, axisScale, fmtCompactTotal, option, seriesKey, seriesLabel, tagPicker, tasks, trendSource, undatedNote } from './metrics-running.js';

const MEASURES = [["turns", "Turns"], ["bytes", "Result bytes"]];

const filters = { outcome: "done", provider: "", tags: [] };
/// Local view state. metrics.js includes `measure` in the section hash so
/// a toggle redraws; the endpoint is not read again.
export const turnsView = { measure: "turns" };

const source = trendSource({
  path: "/api/diagnostics/turns_to_edit",
  params(windowKey) {
    const query = new URLSearchParams({ since: windowKey, outcome: filters.outcome });
    if (filters.provider) query.set("provider", filters.provider);
    if (filters.tags.length) query.set("tags", filters.tags.join(","));
    return query;
  },
  accepts: (payload) => payload && payload.window && Array.isArray(payload.series),
  missingText: "This host answered without a turns-to-first-edit trend.",
  notYetText: "This host's Orbit does not report turns to first edit yet. Upgrade it to see this trend.",
});

/// The endpoint query for `windowKey` under the current filters.
export const turnsQuery = source.query;
/// Fetch the trend for `windowKey`, fenced by metrics-running.js `trendSource`.
export const fetchTurns = source.fetch;
export const isCurrentTurnsPayload = source.isCurrent;

/// "2.5" or "2". A non-finite value is an em dash.
export function fmtTurns(value) {
  if (value == null || !Number.isFinite(Number(value))) return "—";
  const rounded = Math.round(Number(value) * 10) / 10;
  return Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1);
}

/// "1.24M", "860k", "412".
export const fmtBytes = fmtCompactTotal;

function excluded(cell) {
  return (cell.excluded_empty_trace || 0)
    + (cell.excluded_incomplete_trace || 0)
    + (cell.excluded_no_detectable_edit ?? cell.excluded_no_edit ?? 0)
    + (cell.excluded_no_invocation || 0);
}

function coverageCategories(cell) {
  return cell.excluded_empty_trace != null && cell.excluded_no_detectable_edit != null;
}

function excludedDescription(cell) {
  if (!coverageCategories(cell)) {
    return `${cell.excluded_no_edit || 0} with no detectable edit (trace category unavailable), ${cell.excluded_no_invocation || 0} with no implement invocation`;
  }
  return `${cell.excluded_empty_trace} with an empty or unavailable tool trace, ${cell.excluded_incomplete_trace || 0} with a tool trace truncated by the output capture, ${cell.excluded_no_detectable_edit} with a populated trace but no recognized editor, ${cell.excluded_no_invocation || 0} with no implement invocation`;
}

function percent(rate) {
  return rate == null || !Number.isFinite(Number(rate)) ? "—" : `${Math.round(Number(rate) * 100)}%`;
}

function firstEditCoverageExplanation() {
  return "First-edit measurements exclude empty or unavailable tool traces, incomplete captures, populated traces without a recognized editor, and tasks with no implement invocation.";
}

function providerCoverageNote(payload) {
  const providers = payload.available_providers || [];
  if (!providers.length) return "No provider-linked implement invocations in this window.";
  if (!providers.every((entry) => entry.coverage)) return "Provider trace coverage is unavailable from this host.";
  return `Provider coverage (measured / first-invocation tasks; trace rate): ${providers.map((entry) => {
    const coverage = entry.coverage;
    const measured = `${coverage.measured}/${coverage.tasks} measured (${percent(coverage.measured_rate)})`;
    if (coverage.tasks > 0 && coverage.measured === 0 && coverage.empty_trace === coverage.tasks) {
      return `${entry.provider}: no tool telemetry (${measured}; 0% trace coverage; ${coverage.empty_trace} empty traces)`;
    }
    return `${entry.provider}: ${measured}; ${percent(coverage.trace_rate)} trace coverage, ${coverage.empty_trace} empty traces and ${coverage.no_detectable_edit} populated without a recognized editor`;
  }).join(" · ")}.`;
}

/// The filter row: outcome, provider, the tag picker, and the local measure.
export function turnsTools({ segGroup, windowKey, payload, rerender, onChange }) {
  const apply = (update) => {
    Object.assign(filters, update);
    onChange(null);
    source.refetch(windowKey, onChange);
  };
  const providers = (payload && payload.available_providers) || [];
  const provider = el("select", { class: "mx-select", "aria-label": "Provider" }, [
    option("", "All providers", !filters.provider),
    ...providers.map((entry) => {
      const summary = entry.coverage
        ? `${entry.coverage.measured}/${entry.tasks} measured · ${percent(entry.coverage.measured_rate)}`
        : `${entry.tasks} tasks`;
      return option(entry.provider, `${entry.provider} (${summary})`, entry.provider === filters.provider);
    }),
    filters.provider && !providers.some((entry) => entry.provider === filters.provider) ? option(filters.provider, filters.provider, true) : null,
  ]);
  provider.addEventListener("change", () => apply({ provider: provider.value }));

  return [
    segGroup("Task outcome", OUTCOMES, filters.outcome, (key) => key !== filters.outcome && apply({ outcome: key })),
    provider,
    tagPicker({ tags: filters.tags, offered: payload && payload.available_tags, apply }),
    segGroup("Measure", MEASURES, turnsView.measure, (key) => {
      if (key === turnsView.measure) return;
      turnsView.measure = key;
      rerender();
    }),
  ];
}

/// The trend model metrics.js draws: cards, chart, table, or an empty state.
export function turnsModel(payload, { windowKey, shown, bucketTitle, bucketWidthLabel }) {
  const blurb = "Tool calls before the first recognized editor call in a task's first implement invocation, by complexity. Result bytes sum what those calls returned. Empty traces and populated traces without a recognized editor are left out, never scored as zero.";
  if (!payload) return { blurb, cards: [], empty: { title: "Loading turns to first edit…", detail: "", noWiden: true } };
  if (payload.unavailable) return { blurb, cards: [], empty: { title: "Turns to first edit unavailable", detail: payload.unavailable, noWiden: true } };

  const bytes = turnsView.measure === "bytes";
  const meanKey = bytes ? "mean_result_bytes" : "mean_turns";
  const medianKey = bytes ? "median_result_bytes" : "median_turns";
  const format = bytes ? fmtBytes : fmtTurns;
  const seconds = payload.window.bucket_seconds;
  const starts = payload.bucket_starts || [];
  const buckets = starts.map((start) => ({ start }));
  const minTasks = payload.min_tasks;
  const series = (payload.series || []).map((entry) => ({ ...entry, key: seriesKey(entry), label: seriesLabel(entry) }));
  const isShown = (key) => shown[key] !== false;
  const total = payload.total || {};

  const complexityCards = series.map((entry) => ({
    key: entry.key,
    swatch: entry.complexity,
    dashed: entry.cohort === "without",
    label: entry.label,
    tag: "",
    on: isShown(entry.key),
    value: entry.total.n ? format(entry.total[meanKey]) : "—",
    sub: entry.total.n ? `median ${format(entry.total[medianKey])}` : "no measured first edits",
    foot: coverageCategories(entry.total)
      ? `${tasks(entry.total.n)} measured · ${entry.total.excluded_empty_trace} empty traces · ${entry.total.excluded_incomplete_trace || 0} truncated traces · ${entry.total.excluded_no_detectable_edit} populated without an editor · ${entry.total.excluded_no_invocation || 0} without an invocation`
      : `${tasks(entry.total.n)} measured · ${excludedDescription(entry.total)}`,
  }));
  // Keep every complexity and tag cohort in one visible summary tile. The
  // shared responsive card grid changes its column count with the viewport;
  // one composite tile cannot leave a final complexity tile orphaned.
  const cards = [{ grouped: true, label: "By complexity", entries: complexityCards }];

  if (!total.n) {
    const parts = [];
    parts.push(firstEditCoverageExplanation(), providerCoverageNote(payload));
    if (payload.undated_tasks) parts.push(undatedNote(payload.undated_tasks));
    return {
      blurb,
      cards,
      empty: {
        title: `No measured first edits for ${payload.outcome === "done" ? "done" : "closed"} tasks in the last ${windowKey === "all" ? "recorded history" : windowKey}`,
        detail: parts.join(" "),
      },
    };
  }

  const visible = series.filter((entry) => isShown(entry.key));
  let max = 0;
  for (const entry of visible) for (const point of entry.points) if (point.n) max = Math.max(max, point[meanKey]);
  const scale = axisScale(0, max);
  const y = (value) => 100 - Math.min(value, scale.max) / scale.max * 100;
  const yTicks = [];
  for (let v = 0; v <= scale.max + scale.step / 2; v += scale.step) yTicks.push({ top: 100 - v / scale.max * 100, label: format(v) });

  const notes = [
    `${bucketWidthLabel(seconds)}, aligned to the clock`,
    "lines join across buckets where no task closed",
    TERMINAL_TIME_NOTE,
    bytes ? "result bytes before the first edit" : "tool calls before the first edit",
  ];
  if (payload.tags.length) notes.push(`with tag = carries ${payload.tags.join(" or ")} now (current tags, not tags at the time); dashed = without`);
  notes.push(firstEditCoverageExplanation(), providerCoverageNote(payload));
  if (payload.undated_tasks) notes.push(undatedNote(payload.undated_tasks));
  if (payload.truncated) notes.push("older implement invocations were left out of this read");

  return {
    blurb,
    cards,
    chart: {
      kind: "turns",
      buckets,
      seconds,
      yTicks,
      lines: visible.map((entry) => ({
        key: entry.complexity,
        cls: entry.cohort === "without" ? "mx-dashed" : "",
        joined: true,
        points: entry.points.map((point, i) => (point.n ? { i, y: y(point[meanKey]), hollow: point.thin } : null)),
      })),
      notes,
      hollowText: `hollow = fewer than ${minTasks} tasks`,
      aria: `${bytes ? "Mean result bytes" : "Mean tool calls"} before the first file edit by complexity, window ${windowKey}: ${visible.map((entry) => `${entry.label} ${format(entry.total[meanKey])} over ${tasks(entry.total.n)}`).join(", ")}. Use the Table view for per-bucket values.`,
      tip: (index) => {
        const tip = { title: bucketTitle(starts[index], seconds, index === starts.length - 1), rows: [], foot: "Before the first edit · ×n = tasks" };
        let left = 0;
        for (const entry of series) {
          const point = entry.points[index];
          left += excluded(point);
          if (!point.n) continue;
          tip.rows.push({
            key: entry.complexity,
            name: entry.label,
            n: `×${point.n}`,
            low: point.thin,
            value: `turns ${fmtTurns(point.mean_turns)} / ${fmtTurns(point.median_turns)} · bytes ${fmtBytes(point.mean_result_bytes)} / ${fmtBytes(point.median_result_bytes)}`,
          });
        }
        if (!tip.rows.length) tip.empty = "No measured first edit for tasks closed in this bucket.";
        if (tip.rows.some((row) => row.low)) tip.warn = `Amber counts: fewer than ${minTasks} tasks.`;
        if (left) tip.extra = `${tasks(left)} outside the measured sample; see coverage notes for empty traces and unrecognized editors`;
        return tip;
      },
    },
    table: {
      caption: `${bytes ? "Mean result bytes before the first edit" : "Mean tool calls before the first edit"}, median and tasks in parentheses · ${bucketWidthLabel(seconds)} · newest first`,
      heads: series.map((entry) => entry.label),
      rows: starts.map((start, i) => ({
        title: bucketTitle(start, seconds, i === starts.length - 1),
        cells: series.map((entry) => {
          const point = entry.points[i];
          return point.n ? `${format(point[meanKey])} (${format(point[medianKey])}, ${point.n})` : null;
        }),
      })).reverse(),
    },
  };
}

// Analytics › Metrics "Tokens per task" trend (ORB-18364): the mean
// normalized tokens of tasks that closed in each bucket, one line per
// complexity, optionally split into with-tag and without-tag cohorts.
//
// The Tokens tab (ORB-20003) reads the same payload and filters as total tokens
// per bucket, with the Cumulative and Per day views of metrics-running.js.
//
// The trend owns its filters (outcome, invocation scope, implementing model
// and tags) and refetches only its own endpoint when they change; the window
// comes from the shared selector with the rest of the Metrics panel. Chart
// drawing and tooltips stay in metrics.js, which passes its helpers in.

import { el } from './common.js';
import { OUTCOMES, TERMINAL_TIME_NOTE, axisScale, fmtCompactTotal, option, runningModel, seriesKey, seriesLabel, tagPicker, tasks, trendSource, undatedNote } from './metrics-running.js';

const SCOPES = [["all", "All invocations"], ["implement", "Implement only"]];

const filters = { outcome: "done", scope: "all", model: "", tags: [] };

const source = trendSource({
  path: "/api/diagnostics/task_tokens",
  params(windowKey) {
    const query = new URLSearchParams({ since: windowKey, outcome: filters.outcome, scope: filters.scope });
    if (filters.model) query.set("model", filters.model);
    if (filters.tags.length) query.set("tags", filters.tags.join(","));
    return query;
  },
  accepts: (payload) => payload && payload.window && Array.isArray(payload.series),
  missingText: "This host answered without a tokens-per-task trend.",
  notYetText: "This host's Orbit does not report tokens per task yet. Upgrade it to see this trend.",
});

/// The endpoint query for `windowKey` under the current filters.
export const tokensQuery = source.query;
/// Fetch the trend for `windowKey`, fenced by metrics-running.js `trendSource`.
export const fetchTokens = source.fetch;
export const isCurrentTokensPayload = source.isCurrent;

/// "1.24M", "860k", "412".
export const fmtTokens = fmtCompactTotal;

/// The filter row: outcome, scope, implementing model and the tag picker.
export function tokensTools({ segGroup, windowKey, payload, onChange }) {
  // Redraw the controls at once and clear the chart to its loading state, so
  // the visible data never describes the previous filter while the refetch
  // is in flight.
  const apply = (update) => {
    Object.assign(filters, update);
    onChange(null);
    source.refetch(windowKey, onChange);
  };
  const models = (payload && payload.available_models) || [];
  const model = el("select", { class: "mx-select", "aria-label": "Implementing model" }, [
    option("", "All models", !filters.model),
    ...models.map((entry) => option(entry.model, `${entry.model} (${entry.tasks})`, entry.model === filters.model)),
    filters.model && !models.some((entry) => entry.model === filters.model) ? option(filters.model, filters.model, true) : null,
  ]);
  model.addEventListener("change", () => apply({ model: model.value }));

  return [
    segGroup("Task outcome", OUTCOMES, filters.outcome, (key) => key !== filters.outcome && apply({ outcome: key })),
    segGroup("Invocations", SCOPES, filters.scope, (key) => key !== filters.scope && apply({ scope: key })),
    model,
    tagPicker({ tags: filters.tags, offered: payload && payload.available_tags, apply }),
  ];
}

/// The trend model metrics.js draws: cards, chart, table, or an empty state.
export function tokensModel(payload, { windowKey, shown, bucketTitle, bucketWidthLabel }) {
  const blurb = "Mean normalized tokens per task, by complexity, for tasks that closed in each bucket. A task's total sums every managed invocation linked to it, whenever it ran.";
  if (!payload) return { blurb, cards: [], empty: { title: "Loading tokens per task…", detail: "", noWiden: true } };
  if (payload.unavailable) return { blurb, cards: [], empty: { title: "Tokens per task unavailable", detail: payload.unavailable, noWiden: true } };

  const seconds = payload.window.bucket_seconds;
  const starts = payload.bucket_starts || [];
  const buckets = starts.map((start) => ({ start }));
  const minTasks = payload.min_tasks;
  const series = (payload.series || []).map((entry) => ({ ...entry, key: seriesKey(entry), label: seriesLabel(entry) }));
  const isShown = (key) => shown[key] !== false;
  const total = payload.total || {};
  const excluded = (cell) => (cell.excluded_uncovered || 0) + (cell.excluded_no_invocations || 0);

  const cards = series.map((entry) => ({
    key: entry.key,
    swatch: entry.complexity,
    dashed: entry.cohort === "without",
    label: entry.label,
    tag: "",
    on: isShown(entry.key),
    value: fmtTokens(entry.total.mean),
    sub: entry.total.n ? `median ${fmtTokens(entry.total.median)}` : "no tasks",
    foot: `${tasks(entry.total.n)}${excluded(entry.total) ? ` · ${excluded(entry.total)} excluded` : ""}`,
  }));

  if (!total.n) {
    const left = excluded(total);
    return {
      blurb,
      cards,
      empty: {
        title: `No ${payload.outcome === "done" ? "done" : "closed"} tasks with token totals in the last ${windowKey === "all" ? "recorded history" : windowKey}`,
        detail: [left ? `${tasks(left)} excluded: ${total.excluded_uncovered} with an uncovered model, ${total.excluded_no_invocations} with no linked invocation in scope.` : "", undatedNote(payload.undated_tasks)].filter(Boolean).join(" "),
      },
    };
  }

  const visible = series.filter((entry) => isShown(entry.key));
  let max = 0;
  for (const entry of visible) for (const point of entry.points) if (point.n) max = Math.max(max, point.mean);
  const scale = axisScale(0, max);
  const y = (value) => 100 - Math.min(value, scale.max) / scale.max * 100;
  const yTicks = [];
  for (let v = 0; v <= scale.max; v += scale.step) yTicks.push({ top: 100 - v / scale.max * 100, label: fmtTokens(v) });

  const notes = [
    `${bucketWidthLabel(seconds)}, aligned to the clock`,
    "lines join across buckets where no task closed",
    TERMINAL_TIME_NOTE,
    payload.scope === "implement" ? "implement invocations only" : "every linked invocation",
  ];
  if (payload.undated_tasks) notes.push(undatedNote(payload.undated_tasks));
  if (payload.tags.length) notes.push(`with tag = carries ${payload.tags.join(" or ")} now (current tags, not tags at the time); dashed = without`);
  if (payload.model) notes.push(`implementing model ${payload.model}; ${tasks(payload.excluded_other_model)} on other models left out`);
  if (excluded(total)) notes.push(`excluded: ${total.excluded_uncovered} with an uncovered model, ${total.excluded_no_invocations} with no linked invocation`);

  return {
    blurb,
    cards,
    chart: {
      kind: "tokens",
      buckets,
      seconds,
      yTicks,
      lines: visible.map((entry) => ({
        key: entry.complexity,
        cls: entry.cohort === "without" ? "mx-dashed" : "",
        joined: true,
        points: entry.points.map((point, i) => (point.n ? { i, y: y(point.mean), hollow: point.thin } : null)),
      })),
      notes,
      hollowText: `hollow = fewer than ${minTasks} tasks`,
      aria: `Mean normalized tokens per task by complexity, window ${windowKey}: ${visible.map((entry) => `${entry.label} ${fmtTokens(entry.total.mean)} over ${tasks(entry.total.n)}`).join(", ")}. Use the Table view for per-bucket values.`,
      tip: (index) => {
        const tip = { title: bucketTitle(starts[index], seconds, index === starts.length - 1), rows: [], foot: "Normalized tokens per task · ×n = tasks" };
        let left = 0;
        for (const entry of series) {
          const point = entry.points[index];
          left += excluded(point);
          if (!point.n) continue;
          tip.rows.push({ key: entry.complexity, name: entry.label, n: `×${point.n}`, low: point.thin, value: `mean ${fmtTokens(point.mean)} · med ${fmtTokens(point.median)}` });
        }
        if (!tip.rows.length) tip.empty = "No task with a token total closed in this bucket.";
        if (tip.rows.some((row) => row.low)) tip.warn = `Amber counts: fewer than ${minTasks} tasks.`;
        if (left) tip.extra = `${tasks(left)} excluded (uncovered model or no invocation)`;
        return tip;
      },
    },
    table: {
      caption: `Mean normalized tokens per task, median and tasks in parentheses · ${bucketWidthLabel(seconds)} · newest first`,
      heads: series.map((entry) => entry.label),
      rows: starts.map((start, i) => ({
        title: bucketTitle(start, seconds, i === starts.length - 1),
        cells: series.map((entry) => {
          const point = entry.points[i];
          return point.n ? `${fmtTokens(point.mean)} (${fmtTokens(point.median)}, ${point.n})` : null;
        }),
      })).reverse(),
    },
  };
}

/// The Tokens tab's model: total normalized tokens of the tasks that closed in
/// each bucket, per complexity (and tag cohort) and in all, as metrics.js
/// draws them in `countView`.
export function tokenSpendModel(payload, { windowKey, shown, countView, bucketTitle, bucketWidthLabel }) {
  const blurb = "Total normalized tokens of the tasks that closed in each bucket, by complexity. A task's total sums every managed invocation linked to it, whenever it ran. Tokens per task shows the mean.";
  if (!payload) return { blurb, cards: [], empty: { title: "Loading tokens…", detail: "", noWiden: true } };
  if (payload.unavailable) return { blurb, cards: [], empty: { title: "Tokens unavailable", detail: payload.unavailable, noWiden: true } };

  const starts = payload.bucket_starts || [];
  const series = (payload.series || []).map((entry) => ({ ...entry, key: seriesKey(entry), label: seriesLabel(entry) }));
  const isShown = (key) => shown[key] !== false;
  const total = payload.total || {};
  const excluded = (cell) => (cell.excluded_uncovered || 0) + (cell.excluded_no_invocations || 0);
  const sum = (cell) => cell.sum || 0;

  const cards = [
    { key: "all", swatch: "all", label: "Tokens (total)", tag: "", on: isShown("all"), value: fmtTokens(sum(total)), sub: "window total", foot: `${tasks(total.n)}${excluded(total) ? ` · ${excluded(total)} excluded` : ""}` },
    ...series.map((entry) => ({
      key: entry.key,
      swatch: entry.complexity,
      dashed: entry.cohort === "without",
      label: entry.label,
      tag: "",
      on: isShown(entry.key),
      value: fmtTokens(sum(entry.total)),
      sub: "window total",
      foot: `${tasks(entry.total.n)}${excluded(entry.total) ? ` · ${excluded(entry.total)} excluded` : ""}`,
    })),
  ];

  if (!total.n) {
    const left = excluded(total);
    return {
      blurb,
      cards,
      empty: {
        title: `No ${payload.outcome === "done" ? "done" : "closed"} tasks with token totals in the last ${windowKey === "all" ? "recorded history" : windowKey}`,
        detail: [left ? `${tasks(left)} excluded: ${total.excluded_uncovered} with an uncovered model, ${total.excluded_no_invocations} with no linked invocation in scope.` : "", undatedNote(payload.undated_tasks)].filter(Boolean).join(" "),
      },
    };
  }

  const columns = (cells) => cells.map(sum);
  const addColumns = (lists, length) => Array.from({ length }, (_, i) => lists.reduce((sumSoFar, list) => sumSoFar + (list[i] || 0), 0));
  const leadLength = Math.max(0, ...series.map((entry) => (entry.lead_points || []).length));
  const notes = [
    TERMINAL_TIME_NOTE,
    payload.scope === "implement" ? "implement invocations only" : "every linked invocation",
  ];
  if (payload.undated_tasks) notes.push(undatedNote(payload.undated_tasks));
  if (payload.tags.length) notes.push(`with tag = carries ${payload.tags.join(" or ")} now (current tags, not tags at the time); dashed = without`);
  if (payload.model) notes.push(`implementing model ${payload.model}; ${tasks(payload.excluded_other_model)} on other models left out`);
  if (excluded(total)) notes.push(`excluded: ${total.excluded_uncovered} with an uncovered model, ${total.excluded_no_invocations} with no linked invocation`);

  return {
    blurb,
    cards,
    ...runningModel({
      mode: countView,
      kind: "tokens",
      windowKey,
      seconds: payload.window.bucket_seconds,
      starts,
      series: [
        {
          key: "all",
          swatch: "all",
          label: "Tokens (total)",
          strong: true,
          on: isShown("all"),
          values: addColumns(series.map((entry) => columns(entry.points)), starts.length),
          lead: addColumns(series.map((entry) => columns(entry.lead_points || [])), leadLength),
        },
        ...series.map((entry) => ({
          key: entry.key,
          swatch: entry.complexity,
          label: entry.label,
          dashed: entry.cohort === "without",
          on: isShown(entry.key),
          values: columns(entry.points),
          lead: columns(entry.lead_points || []),
        })),
      ],
      fmt: fmtTokens,
      noun: "tokens",
      subject: "Total normalized tokens of closed tasks",
      notes,
      bucketTitle,
      bucketWidthLabel,
    }),
  };
}

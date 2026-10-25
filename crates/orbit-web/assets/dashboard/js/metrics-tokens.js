// Analytics › Metrics "Tokens per task" trend (ORB-18364): the mean
// normalized tokens of tasks that closed in each bucket, one line per
// complexity, optionally split into with-tag and without-tag cohorts.
//
// The trend owns its filters (outcome, invocation scope, implementing model
// and tags) and refetches only its own endpoint when they change; the window
// comes from the shared selector with the rest of the Metrics panel. Chart
// drawing and tooltips stay in metrics.js, which passes its helpers in.

import { el, fetchJson, getWorkspaceRevision } from './common.js';

const COMPLEXITY_LABELS = { low: "Low", medium: "Medium", hard: "Hard", xhard: "Xhard", unset: "Unassessed" };
const OUTCOMES = [["done", "Done"], ["all", "All closed"]];
const SCOPES = [["all", "All invocations"], ["implement", "Implement only"]];

const filters = { outcome: "done", scope: "all", model: "", tags: [] };
let sequence = 0;

/// The endpoint query for `windowKey` under the current filters.
export function tokensQuery(windowKey) {
  const query = new URLSearchParams({ since: windowKey, outcome: filters.outcome, scope: filters.scope });
  if (filters.model) query.set("model", filters.model);
  if (filters.tags.length) query.set("tags", filters.tags.join(","));
  return `/api/diagnostics/task_tokens?${query}`;
}

/// Fetch the trend for `windowKey`. A failed read, or a host whose Orbit
/// predates the endpoint, answers `{ unavailable }` so the other trends still
/// render. Every result carries a fence (generation, workspace/host visit
/// revision, window and complete query) captured at request time; both the
/// filter refetch and the full-panel refresh share `sequence`, so whichever
/// starts later retires the other.
export function fetchTokens(windowKey) {
  const fence = { generation: ++sequence, revision: getWorkspaceRevision(), windowKey, query: tokensQuery(windowKey) };
  const query = fence.query;
  return fetchJson(query).then(
    (payload) => (payload && payload.window && Array.isArray(payload.series)
      ? { ...payload, query, tokenFence: fence }
      : { query, unavailable: "This host answered without a tokens-per-task trend.", tokenFence: fence }),
    (error) => ({
      query,
      unavailable: error.status === 404 ? "This host's Orbit does not report tokens per task yet. Upgrade it to see this trend." : error.message,
      tokenFence: fence,
    }),
  );
}

/// True only when `payload` is the newest tokens request and its visit,
/// window and complete filter query still match current state. Unavailable
/// and error replies carry the fence too, so they are gated the same way.
export function isCurrentTokensPayload(payload, windowKey) {
  const fence = payload && payload.tokenFence;
  if (!fence) return false;
  return fence.generation === sequence
    && fence.revision === getWorkspaceRevision()
    && fence.windowKey === windowKey
    && fence.query === tokensQuery(windowKey);
}

/// Refetch after a filter change; `apply` runs only for a current request.
function refetch(windowKey, apply) {
  fetchTokens(windowKey).then((payload) => {
    if (isCurrentTokensPayload(payload, windowKey)) apply(payload);
  });
}

/// "1.24M", "860k", "412".
export function fmtTokens(value) {
  if (value == null || !Number.isFinite(Number(value))) return "—";
  const n = Number(value);
  if (n >= 1e9) return `${(n / 1e9).toFixed(2)}B`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(n >= 1e7 ? 1 : 2)}M`;
  if (n >= 1e3) return `${Math.round(n / 1e3)}k`;
  return String(Math.round(n));
}

function tasks(n) {
  return `${n} ${n === 1 ? "task" : "tasks"}`;
}

function seriesKey(series) {
  return series.cohort === "all" ? series.complexity : `${series.complexity}-${series.cohort}`;
}

function seriesLabel(series) {
  const name = COMPLEXITY_LABELS[series.complexity] || series.complexity;
  if (series.cohort === "with") return `${name} · with tag`;
  if (series.cohort === "without") return `${name} · without`;
  return name;
}

/// A 1–2–5 axis from zero past `max`, in about four steps.
function tokenScale(max) {
  const raw = Math.max(max, 1) * 1.08 / 4;
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 5, 10].map((m) => m * magnitude).find((candidate) => candidate >= raw);
  return { step, max: Math.ceil(Math.max(max, 1) * 1.08 / step) * step };
}

/// The filter row: outcome, scope, implementing model and the tag picker.
export function tokensTools({ segGroup, windowKey, payload, onChange }) {
  // Redraw the controls at once and clear the chart to its loading state, so
  // the visible data never describes the previous filter while the refetch
  // is in flight.
  const apply = (update) => {
    Object.assign(filters, update);
    onChange(null);
    refetch(windowKey, onChange);
  };
  const models = (payload && payload.available_models) || [];
  const model = el("select", { class: "mx-select", "aria-label": "Implementing model" }, [
    option("", "All models", !filters.model),
    ...models.map((entry) => option(entry.model, `${entry.model} (${entry.tasks})`, entry.model === filters.model)),
    filters.model && !models.some((entry) => entry.model === filters.model) ? option(filters.model, filters.model, true) : null,
  ]);
  model.addEventListener("change", () => apply({ model: model.value }));

  const available = ((payload && payload.available_tags) || []).filter((entry) => !filters.tags.includes(entry.tag));
  const picker = el("select", { class: "mx-select", "aria-label": "Compare a tag" }, [
    option("", filters.tags.length ? "Add a tag…" : "Compare a tag…", true),
    ...available.map((entry) => option(entry.tag, `${entry.tag} (${entry.tasks})`, false)),
  ]);
  picker.addEventListener("change", () => {
    if (picker.value) apply({ tags: [...filters.tags, picker.value].sort() });
  });
  const chips = filters.tags.map((tag) => {
    const chip = el("button", { class: "mx-chip", type: "button", title: `Stop comparing ${tag}`, "aria-label": `Remove tag ${tag}` }, [tag, el("span", { "aria-hidden": "true", text: "×" })]);
    chip.addEventListener("click", () => apply({ tags: filters.tags.filter((other) => other !== tag) }));
    return chip;
  });

  return [
    segGroup("Task outcome", OUTCOMES, filters.outcome, (key) => key !== filters.outcome && apply({ outcome: key })),
    segGroup("Invocations", SCOPES, filters.scope, (key) => key !== filters.scope && apply({ scope: key })),
    model,
    el("div", { class: "mx-tags", role: "group", "aria-label": "Tag cohorts" }, [picker, ...chips]),
  ];
}

function option(value, text, selected) {
  const node = el("option", { text });
  node.value = value;
  node.selected = selected;
  return node;
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
        detail: left ? `${tasks(left)} excluded: ${total.excluded_uncovered} with an uncovered model, ${total.excluded_no_invocations} with no linked invocation in scope.` : "",
      },
    };
  }

  const visible = series.filter((entry) => isShown(entry.key));
  let max = 0;
  for (const entry of visible) for (const point of entry.points) if (point.n) max = Math.max(max, point.mean);
  const scale = tokenScale(max);
  const y = (value) => 100 - Math.min(value, scale.max) / scale.max * 100;
  const yTicks = [];
  for (let v = 0; v <= scale.max; v += scale.step) yTicks.push({ top: 100 - v / scale.max * 100, label: fmtTokens(v) });

  const notes = [
    `${bucketWidthLabel(seconds)}, aligned to the clock`,
    "lines skip buckets where no task closed",
    "time = the task's last write after it closed",
    payload.scope === "implement" ? "implement invocations only" : "every linked invocation",
  ];
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

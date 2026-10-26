// Analytics › Metrics "Turns to first edit" trend (ORB-18542): tool calls
// and the result bytes returned before the first file edit of each task's
// first implement invocation, one line per complexity, optionally split
// into with-tag and without-tag cohorts.
//
// The trend owns its filters (outcome, provider and tags) and refetches
// only its own endpoint when they change. Turns versus result bytes is
// local and redraws from the cached payload. The window comes from the
// shared selector. Chart drawing and tooltips stay in metrics.js.

import { el, fetchJson, getWorkspaceRevision } from './common.js';

const COMPLEXITY_LABELS = { low: "Low", medium: "Medium", hard: "Hard", xhard: "Xhard", unset: "Unassessed" };
const OUTCOMES = [["done", "Done"], ["all", "All closed"]];
const MEASURES = [["turns", "Turns"], ["bytes", "Result bytes"]];
const MAX_TAGS = 8;

const filters = { outcome: "done", provider: "", tags: [] };
/// Local view state. metrics.js includes `measure` in the section hash so
/// a toggle redraws; the endpoint is not read again.
export const turnsView = { measure: "turns" };
let sequence = 0;

/// The endpoint query for `windowKey` under the current filters.
export function turnsQuery(windowKey) {
  const query = new URLSearchParams({ since: windowKey, outcome: filters.outcome });
  if (filters.provider) query.set("provider", filters.provider);
  if (filters.tags.length) query.set("tags", filters.tags.join(","));
  return `/api/diagnostics/turns_to_edit?${query}`;
}

/// Fetch the trend for `windowKey`. A failed read, or a host whose Orbit
/// predates the endpoint, answers `{ unavailable }` so the other trends still
/// render. Every result carries a fence (generation, workspace/host visit
/// revision, window and complete query) captured at request time; both the
/// filter refetch and the full-panel refresh share `sequence`, so whichever
/// starts later retires the other.
export function fetchTurns(windowKey) {
  const fence = { generation: ++sequence, revision: getWorkspaceRevision(), windowKey, query: turnsQuery(windowKey) };
  const query = fence.query;
  return fetchJson(query).then(
    (payload) => (payload && payload.window && Array.isArray(payload.series)
      ? { ...payload, query, turnsFence: fence }
      : { query, unavailable: "This host answered without a turns-to-first-edit trend.", turnsFence: fence }),
    (error) => ({
      query,
      unavailable: error.status === 404 ? "This host's Orbit does not report turns to first edit yet. Upgrade it to see this trend." : error.message,
      turnsFence: fence,
    }),
  );
}

/// True only when `payload` is the newest turns request and its visit,
/// window and complete filter query still match current state. Unavailable
/// and error replies carry the fence too, so they are gated the same way.
export function isCurrentTurnsPayload(payload, windowKey) {
  const fence = payload && payload.turnsFence;
  if (!fence) return false;
  return fence.generation === sequence
    && fence.revision === getWorkspaceRevision()
    && fence.windowKey === windowKey
    && fence.query === turnsQuery(windowKey);
}

/// Refetch after a filter change; `apply` runs only for a current request.
function refetch(windowKey, apply) {
  fetchTurns(windowKey).then((payload) => {
    if (isCurrentTurnsPayload(payload, windowKey)) apply(payload);
  });
}

/// "2.5" or "2". A non-finite value is an em dash.
export function fmtTurns(value) {
  if (value == null || !Number.isFinite(Number(value))) return "—";
  const rounded = Math.round(Number(value) * 10) / 10;
  return Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1);
}

/// "1.24M", "860k", "412".
export function fmtBytes(value) {
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

function excluded(cell) {
  return (cell.excluded_no_edit || 0) + (cell.excluded_no_invocation || 0);
}

/// A 1–2–5 axis from zero past `max`, in about four steps.
function axisScale(max) {
  const raw = Math.max(max, 1) * 1.08 / 4;
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 5, 10].map((m) => m * magnitude).find((candidate) => candidate >= raw);
  return { step, max: Math.ceil(Math.max(max, 1) * 1.08 / step) * step };
}

/// The filter row: outcome, provider, the tag picker, and the local measure.
export function turnsTools({ segGroup, windowKey, payload, rerender, onChange }) {
  const apply = (update) => {
    Object.assign(filters, update);
    onChange(null);
    refetch(windowKey, onChange);
  };
  const providers = (payload && payload.available_providers) || [];
  const provider = el("select", { class: "mx-select", "aria-label": "Provider" }, [
    option("", "All providers", !filters.provider),
    ...providers.map((entry) => option(entry.provider, `${entry.provider} (${entry.tasks})`, entry.provider === filters.provider)),
    filters.provider && !providers.some((entry) => entry.provider === filters.provider) ? option(filters.provider, filters.provider, true) : null,
  ]);
  provider.addEventListener("change", () => apply({ provider: provider.value }));

  const atTagLimit = filters.tags.length >= MAX_TAGS;
  const available = atTagLimit ? [] : ((payload && payload.available_tags) || []).filter((entry) => !filters.tags.includes(entry.tag));
  const picker = el("select", { class: "mx-select", "aria-label": "Compare a tag" }, [
    option("", atTagLimit ? `Maximum of ${MAX_TAGS} tags selected` : filters.tags.length ? "Add a tag…" : "Compare a tag…", true),
    ...available.map((entry) => option(entry.tag, `${entry.tag} (${entry.tasks})`, false)),
  ]);
  picker.addEventListener("change", () => {
    if (picker.value && filters.tags.length < MAX_TAGS && available.some((entry) => entry.tag === picker.value)) {
      apply({ tags: [...filters.tags, picker.value].sort() });
    }
  });
  const chips = filters.tags.map((tag) => {
    const chip = el("button", { class: "mx-chip", type: "button", title: `Stop comparing ${tag}`, "aria-label": `Remove tag ${tag}` }, [tag, el("span", { "aria-hidden": "true", text: "×" })]);
    chip.addEventListener("click", () => apply({ tags: filters.tags.filter((other) => other !== tag) }));
    return chip;
  });

  return [
    segGroup("Task outcome", OUTCOMES, filters.outcome, (key) => key !== filters.outcome && apply({ outcome: key })),
    provider,
    el("div", { class: "mx-tags", role: "group", "aria-label": "Tag cohorts" }, [picker, ...chips]),
    segGroup("Measure", MEASURES, turnsView.measure, (key) => {
      if (key === turnsView.measure) return;
      turnsView.measure = key;
      rerender();
    }),
  ];
}

function option(value, text, selected) {
  const node = el("option", { text });
  node.value = value;
  node.selected = selected;
  return node;
}

/// The trend model metrics.js draws: cards, chart, table, or an empty state.
export function turnsModel(payload, { windowKey, shown, bucketTitle, bucketWidthLabel }) {
  const blurb = "Tool calls before the first file edit in a task's first implement invocation, by complexity. Result bytes sum what those calls returned. An invocation with no detectable edit is left out, not scored as zero.";
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

  const cards = series.map((entry) => ({
    key: entry.key,
    swatch: entry.complexity,
    dashed: entry.cohort === "without",
    label: entry.label,
    tag: "",
    on: isShown(entry.key),
    value: entry.total.n ? format(entry.total[meanKey]) : "—",
    sub: entry.total.n ? `median ${format(entry.total[medianKey])}` : "no tasks",
    foot: `${tasks(entry.total.n)}${excluded(entry.total) ? ` · ${excluded(entry.total)} excluded` : ""}`,
  }));

  if (!total.n) {
    const left = excluded(total);
    const other = payload.excluded_other_provider || 0;
    const parts = [];
    if (left) parts.push(`${tasks(left)} excluded: ${total.excluded_no_edit || 0} with no detectable edit, ${total.excluded_no_invocation || 0} with no implement invocation`);
    if (other) parts.push(`${tasks(other)} on another provider`);
    return {
      blurb,
      cards,
      empty: {
        title: `No ${payload.outcome === "done" ? "done" : "closed"} tasks with a detectable edit in the last ${windowKey === "all" ? "recorded history" : windowKey}`,
        detail: parts.join(". "),
      },
    };
  }

  const visible = series.filter((entry) => isShown(entry.key));
  let max = 0;
  for (const entry of visible) for (const point of entry.points) if (point.n) max = Math.max(max, point[meanKey]);
  const scale = axisScale(max);
  const y = (value) => 100 - Math.min(value, scale.max) / scale.max * 100;
  const yTicks = [];
  for (let v = 0; v <= scale.max + scale.step / 2; v += scale.step) yTicks.push({ top: 100 - v / scale.max * 100, label: format(v) });

  const notes = [
    `${bucketWidthLabel(seconds)}, aligned to the clock`,
    "lines join across buckets where no task closed",
    "time = the task's last write after it closed",
    bytes ? "result bytes before the first edit" : "tool calls before the first edit",
  ];
  if (payload.tags.length) notes.push(`with tag = carries ${payload.tags.join(" or ")} now (current tags, not tags at the time); dashed = without`);
  if (payload.provider) notes.push(`provider ${payload.provider}; ${tasks(payload.excluded_other_provider || 0)} whose first implement invocation ran on another provider`);
  if (excluded(total)) notes.push(`excluded: ${total.excluded_no_edit || 0} with no detectable edit, ${total.excluded_no_invocation || 0} with no implement invocation`);
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
        if (!tip.rows.length) tip.empty = "No task with a detectable edit closed in this bucket.";
        if (tip.rows.some((row) => row.low)) tip.warn = `Amber counts: fewer than ${minTasks} tasks.`;
        if (left) tip.extra = `${tasks(left)} excluded (no detectable edit, or no implement invocation)`;
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

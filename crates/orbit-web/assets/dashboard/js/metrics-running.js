// Analytics › Metrics running views (ORB-20003) for the count trends (Lines
// changed, Tasks completed, Defects and Tokens): Per bucket, Cumulative and
// Per day.
//
// Cumulative is a running total from the start of the window, so each series'
// last point is that series' sum since the window started; empty buckets stay
// flat. Per day is the sum of the 24 hours ending at each bucket's close,
// filled from the endpoint's lead buckets (history before the window) so the
// first points are complete; a host that sends none draws those leading points
// hollow. The views are derived here from the bucket series; chart drawing
// stays in metrics.js.

import { el, fetchJson, getWorkspaceRevision } from './common.js';

const DAY_SECONDS = 86400;

export const COUNT_VIEWS = [["bucket", "Per bucket"], ["cumulative", "Cumulative"], ["day", "Per day"]];
// The hash query parameter that remembers each count trend's view.
const VIEW_PARAMS = { completed: "completed_view", defects: "defects_view", "tokens-total": "tokens_view", diff: "diff_view" };

export const TERMINAL_TIME_NOTE = "time = when the task entered its closed status, from its recorded history; later edits do not move it";

/// The footnote for closed tasks whose history records no time to count them
/// at, or null when there are none (an older host reports no count).
export function undatedNote(count) {
  if (!count) return null;
  return `${count} closed ${count === 1 ? "task has" : "tasks have"} no recorded close time and ${count === 1 ? "is" : "are"} left out`;
}

export function isCountTrend(trend) {
  return Object.hasOwn(VIEW_PARAMS, trend);
}

/// The views a hash query remembers, by trend; unknown values are dropped.
export function countViewsFromQuery(query) {
  const chosen = {};
  for (const [trend, param] of Object.entries(VIEW_PARAMS)) {
    const value = query.get(param);
    if (COUNT_VIEWS.some(([key]) => key === value)) chosen[trend] = value;
  }
  return chosen;
}

/// `&param=value` for each remembered view, for the canonical Metrics hash.
export function countViewsHashSuffix(query) {
  return Object.entries(countViewsFromQuery(query)).map(([trend, value]) => `&${VIEW_PARAMS[trend]}=${value}`).join("");
}

/// The current address with `trend` remembering `view`, for an in-place rewrite.
export function countViewHash(hash, trend, view) {
  const [route, queryString = ""] = String(hash || "").replace(/^#/, "").split("?");
  const query = new URLSearchParams(queryString);
  query.set(VIEW_PARAMS[trend], view);
  return `#${route}?${query}`;
}

/// The view a window opens on: Per day for the long windows, else Cumulative.
export function defaultCountView(windowKey) {
  return windowKey === "7d" || windowKey === "30d" ? "day" : "cumulative";
}

/// A trailing 24 hours needs buckets that tile a day; the weekly ladder
/// widths do not. `null` seconds (not loaded yet) offers every view.
export function availableCountViews(seconds) {
  const tiles = seconds == null || (seconds <= DAY_SECONDS && DAY_SECONDS % seconds === 0);
  return COUNT_VIEWS.filter(([key]) => key !== "day" || tiles);
}

export function resolveCountView(chosen, windowKey, seconds) {
  const available = availableCountViews(seconds).map(([key]) => key);
  if (available.includes(chosen)) return chosen;
  const fallback = defaultCountView(windowKey);
  return available.includes(fallback) ? fallback : "cumulative";
}

function runningTotal(values) {
  let sum = 0;
  return values.map((value) => (sum += value));
}

/// The sum of the `perDay` buckets ending at each bucket, with the leading
/// `lead` buckets before the window; `incomplete[i]` is true when fewer than
/// `perDay` buckets of history exist for point `i`.
function trailingDay(values, lead, perDay) {
  const all = [...lead, ...values];
  const offset = lead.length;
  const sums = [];
  const incomplete = [];
  for (let i = 0; i < values.length; i++) {
    const end = offset + i;
    let sum = 0;
    for (let j = Math.max(0, end - perDay + 1); j <= end; j++) sum += all[j];
    sums.push(sum);
    incomplete.push(end - perDay + 1 < 0);
  }
  return { sums, incomplete };
}

/// A 1–2–5 axis covering `[min, max]` and zero, in about four steps. `floor`
/// is the smallest top the axis may have (1 by default, so an empty chart
/// still draws a scale).
export function axisScale(min, max, floor = 1) {
  const low = Math.min(0, min);
  const high = Math.max(floor, max);
  const raw = (high - low) * 1.08 / 4;
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 5, 10].map((m) => m * magnitude).find((candidate) => candidate >= raw);
  return { step, min: low < 0 ? Math.floor(low * 1.08 / step) * step : 0, max: Math.ceil(high * 1.08 / step) * step };
}

const MODE_WORDS = {
  bucket: "per bucket",
  cumulative: "cumulative",
  day: "per day (trailing 24h)",
};

/// The chart and table models for one count trend in `mode`. `series` is
/// `[{ key, label, swatch, dashed, strong, on, values, lead }]`: `values` is
/// the per-bucket quantity (one per entry of `starts`), `lead` the same
/// quantity for the buckets before the window, oldest first. `fmt` formats a
/// value, `noun` names it ("lines", "tasks"), `tipExtra(index, valueOf)`
/// may add a tooltip line (`valueOf(key)` is the plotted value at `index`) and
/// `notes` are trend-specific legend lines.
export function runningModel({ mode, kind, windowKey, seconds, starts, series, fmt, noun, subject, notes = [], tipExtra, bucketTitle, bucketWidthLabel }) {
  const perDay = seconds <= DAY_SECONDS && DAY_SECONDS % seconds === 0 ? DAY_SECONDS / seconds : 1;
  const last = starts.length - 1;
  const plotted = series.map((entry) => {
    if (mode === "cumulative") return { ...entry, points: runningTotal(entry.values), incomplete: [] };
    if (mode === "day") {
      const { sums, incomplete } = trailingDay(entry.values, entry.lead || [], perDay);
      return { ...entry, points: sums, incomplete };
    }
    return { ...entry, points: entry.values, incomplete: [] };
  });
  const visible = plotted.filter((entry) => entry.on);
  let min = 0;
  let max = 0;
  for (const entry of visible) {
    for (const value of entry.points) {
      min = Math.min(min, value);
      max = Math.max(max, value);
    }
  }
  const scale = axisScale(min, max);
  const span = scale.max - scale.min;
  const y = (value) => 100 - (Math.min(Math.max(value, scale.min), scale.max) - scale.min) / span * 100;
  const yTicks = [];
  for (let v = scale.min; v <= scale.max; v += scale.step) yTicks.push({ top: y(v), label: fmt(v) });

  const anyIncomplete = visible.some((entry) => entry.incomplete.some(Boolean));
  const modeNotes = [`${bucketWidthLabel(seconds)}, aligned to the clock`];
  if (mode === "cumulative") {
    modeNotes.push(`running total from the start of the window; each line's last point is that series' sum since the window started`, "empty buckets stay flat");
  } else if (mode === "day") {
    modeNotes.push(perDay === 1
      ? "each point is one day, the same as per bucket; the last day is still in progress"
      : "each point = the sum of the 24 hours ending at the bucket's close; the last bucket is still in progress");
    modeNotes.push(anyIncomplete ? "history before the window is missing, so the leading points sum less than 24h" : "history before the window fills the first points");
  }
  const pointsOf = (entry) => entry.points.map((value, i) => ({ i, y: y(value), hollow: !!entry.incomplete[i] }));

  return {
    chart: {
      kind,
      buckets: starts.map((start) => ({ start })),
      seconds,
      yTicks,
      lines: visible.map((entry) => ({ key: entry.swatch, cls: entry.dashed ? "mx-dashed" : "", points: pointsOf(entry) })),
      notes: [...modeNotes, ...notes],
      hollowText: anyIncomplete ? "hollow = fewer than 24h of history" : null,
      aria: `${subject}, ${MODE_WORDS[mode]}, window ${windowKey}: ${visible.map((entry) => `${entry.label} ${fmt(entry.points[last] ?? 0)}`).join(", ")}. Use the Table view for per-bucket values.`,
      tip: (index) => {
        const tip = {
          title: bucketTitle(starts[index], seconds, index === last),
          rows: plotted.filter((entry) => entry.on).map((entry) => ({ key: entry.swatch, dashed: entry.dashed, name: entry.label, strong: entry.strong, value: fmt(entry.points[index]) })),
          foot: mode === "cumulative" ? `Running total of ${noun} through this bucket` : mode === "day" ? `${noun[0].toUpperCase()}${noun.slice(1)} in the 24 hours ending at this bucket's close` : `${noun[0].toUpperCase()}${noun.slice(1)} in this bucket`,
        };
        if (visible.some((entry) => entry.incomplete[index])) tip.warn = "Hollow: fewer than 24h of history behind this point.";
        const extra = tipExtra && tipExtra(index, (key) => (plotted.find((entry) => entry.key === key) || { points: [] }).points[index]);
        if (extra) tip.extra = extra;
        return tip;
      },
    },
    table: {
      caption: `${subject}, ${MODE_WORDS[mode]} · ${bucketWidthLabel(seconds)} · newest first`,
      heads: plotted.map((entry) => entry.label),
      rows: starts.map((start, i) => ({
        title: bucketTitle(start, seconds, i === last),
        cells: plotted.map((entry) => fmt(entry.points[i])),
      })).reverse(),
    },
  };
}

/// The request and stale-response fence one trend shares between its own
/// filter refetch and the full-panel refresh. `params(windowKey)` builds the
/// endpoint's `URLSearchParams` from the trend's current filters and
/// `accepts(payload)` says whether a reply has the shape the trend draws. A
/// failed read, or a host whose Orbit predates the endpoint, answers
/// `{ unavailable }` so the other trends still render. Every result carries a
/// fence (generation, workspace/host visit revision, window and complete
/// query) captured at request time; whichever request starts later retires
/// the other, and `isCurrent` gates unavailable and error replies the same way.
export function trendSource({ path, params, accepts, missingText, notYetText }) {
  let sequence = 0;
  const query = (windowKey) => `${path}?${params(windowKey)}`;
  const fetchTrend = (windowKey) => {
    const fence = { generation: ++sequence, revision: getWorkspaceRevision(), windowKey, query: query(windowKey) };
    const requested = fence.query;
    return fetchJson(requested).then(
      (payload) => (accepts(payload)
        ? { ...payload, query: requested, trendFence: fence }
        : { query: requested, unavailable: missingText, trendFence: fence }),
      (error) => ({ query: requested, unavailable: error.status === 404 ? notYetText : error.message, trendFence: fence }),
    );
  };
  /// True only when `payload` is the newest request and its visit, window and
  /// complete query still match current state.
  const isCurrent = (payload, windowKey) => {
    const fence = payload && payload.trendFence;
    if (!fence) return false;
    return fence.generation === sequence
      && fence.revision === getWorkspaceRevision()
      && fence.windowKey === windowKey
      && fence.query === query(windowKey);
  };
  /// Refetch after a filter change; `apply` runs only for a current request.
  const refetch = (windowKey, apply) => {
    fetchTrend(windowKey).then((payload) => {
      if (isCurrent(payload, windowKey)) apply(payload);
    });
  };
  return { query, fetch: fetchTrend, isCurrent, refetch };
}

export const COMPLEXITY_LABELS = { low: "Low", medium: "Medium", hard: "Hard", xhard: "Xhard", unset: "Unassessed" };
/// The complexity filter's choices, for the endpoints that take one.
export const COMPLEXITIES = [["", "All complexities"], ["low", "Low"], ["medium", "Medium"], ["hard", "Hard"], ["xhard", "Xhard"], ["unassessed", "Unassessed"]];
export const OUTCOMES = [["done", "Done"], ["all", "All closed"]];
const MAX_TAGS = 8;

export function tasks(n) {
  return `${n} ${n === 1 ? "task" : "tasks"}`;
}

/// The series key and label of one complexity (and tag cohort) series.
export function seriesKey(series) {
  return series.cohort === "all" ? series.complexity : `${series.complexity}-${series.cohort}`;
}

export function seriesLabel(series) {
  const name = COMPLEXITY_LABELS[series.complexity] || series.complexity;
  if (series.cohort === "with") return `${name} · with tag`;
  if (series.cohort === "without") return `${name} · without`;
  return name;
}

/// "1.24M", "860k", "412": a count or byte total in compact units.
export function fmtCompactTotal(value) {
  if (value == null || !Number.isFinite(Number(value))) return "—";
  const n = Number(value);
  if (n >= 1e9) return `${(n / 1e9).toFixed(2)}B`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(n >= 1e7 ? 1 : 2)}M`;
  if (n >= 1e3) return `${Math.round(n / 1e3)}k`;
  return String(Math.round(n));
}

export function option(value, text, selected) {
  const node = el("option", { text });
  node.value = value;
  node.selected = selected;
  return node;
}

/// The "Compare a tag" picker with a removable chip per chosen tag, up to
/// eight. `tags` are the chosen tags, `offered` the payload's
/// `available_tags` and `apply({ tags })` stores a new choice.
export function tagPicker({ tags, offered, apply }) {
  const atTagLimit = tags.length >= MAX_TAGS;
  const available = atTagLimit ? [] : (offered || []).filter((entry) => !tags.includes(entry.tag));
  const picker = el("select", { class: "mx-select", "aria-label": "Compare a tag" }, [
    option("", atTagLimit ? `Maximum of ${MAX_TAGS} tags selected` : tags.length ? "Add a tag…" : "Compare a tag…", true),
    ...available.map((entry) => option(entry.tag, `${entry.tag} (${entry.tasks})`, false)),
  ]);
  picker.addEventListener("change", () => {
    if (picker.value && tags.length < MAX_TAGS && available.some((entry) => entry.tag === picker.value)) {
      apply({ tags: [...tags, picker.value].sort() });
    }
  });
  const chips = tags.map((tag) => {
    const chip = el("button", { class: "mx-chip", type: "button", "aria-description": `Stop comparing ${tag}`, "aria-label": `Remove tag ${tag}` }, [tag, el("span", { "aria-hidden": "true", text: "×" })]);
    chip.addEventListener("click", () => apply({ tags: tags.filter((other) => other !== tag) }));
    return chip;
  });
  return el("div", { class: "mx-tags", role: "group", "aria-label": "Tag cohorts" }, [picker, ...chips]);
}

/// "43.9m", "1h 05m", "45s" — minutes carry one decimal so close trends stay
/// distinguishable.
export function fmtMs(ms) {
  if (ms == null || !Number.isFinite(Number(ms))) return "—";
  const value = Number(ms);
  if (value < 60000) return `${Math.round(value / 1000)}s`;
  const minutes = value / 60000;
  if (minutes < 60) return `${minutes.toFixed(1)}m`;
  let hours = Math.floor(minutes / 60);
  let rest = Math.round(minutes - hours * 60);
  if (rest === 60) { hours += 1; rest = 0; }
  return `${hours}h ${String(rest).padStart(2, "0")}m`;
}

export function previousLabel(windowKeyValue) {
  return windowKeyValue === "all" ? "" : `prev ${windowKeyValue}`;
}

// mode "duration": higher is worse; "count": higher is better; "defects":
// a count where higher is worse.
export function delta(current, previous, mode) {
  if (current == null) return null;
  // Nothing then and nothing now is no change, not a new series.
  if (!current && !previous) return null;
  if (previous == null || !previous) return { text: "new", cls: "flat" };
  const ratio = (current - previous) / previous * 100;
  const abs = Math.abs(ratio);
  const text = `${ratio >= 0 ? "▲" : "▼"} ${abs >= 10 ? Math.round(abs) : abs.toFixed(1)}%`;
  const higherIsWorse = mode === "duration" || mode === "defects";
  const worse = higherIsWorse ? ratio > 5 : ratio < -5;
  const better = higherIsWorse ? ratio < -5 : ratio > 5;
  const word = mode === "duration" ? (ratio >= 0 ? "slower" : "faster") : (ratio >= 0 ? "more" : "fewer");
  return { text: `${text} ${word}`, cls: worse ? "bad" : better ? "good" : "flat" };
}

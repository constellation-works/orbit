// Analytics › Metrics running views (ORB-20003) for the count trends (Lines
// changed, Tasks completed, Defects and Tokens): Per bucket, Cumulative and
// Per day.
//
// Cumulative is a running total from the start of the window, so its last
// point is the card total; empty buckets stay flat. Per day is the sum of the
// 24 hours ending at each bucket's close, filled from the endpoint's lead
// buckets (history before the window) so the first points are complete; a
// host that sends none draws those leading points hollow. The views are
// derived here from the bucket series; chart drawing stays in metrics.js.

const DAY_SECONDS = 86400;

export const COUNT_VIEWS = [["bucket", "Per bucket"], ["cumulative", "Cumulative"], ["day", "Per day"]];
// The hash query parameter that remembers each count trend's view.
const VIEW_PARAMS = { completed: "completed_view", defects: "defects_view", "tokens-total": "tokens_view", diff: "diff_view" };

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

/// A 1–2–5 axis covering `[min, max]` and zero, in about four steps.
function axisScale(min, max) {
  const low = Math.min(0, min);
  const high = Math.max(1, max);
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
    modeNotes.push(`running total from the start of the window; last point = card total`, "empty buckets stay flat");
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

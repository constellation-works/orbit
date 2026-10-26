// Analytics › Metrics "Delivery friction" trend (ORB-18384): step and final
// recoveries, PR auto-fixes, merge conflicts, failed CI, cost and tokens in
// each bucket, as counts or as rates per task done in the bucket.
//
// The family picker and the complexity and crew filters are the endpoint's
// and refetch only this trend; the count/rate measure is local and redraws
// from the cached payload. The filters apply only to the families whose
// occurrences belong to a task; the payload says which. The window comes
// from the shared selector; chart drawing and tooltips stay in metrics.js,
// which passes its helpers in.

import { el, fetchJson, getWorkspaceRevision } from './common.js';

const FAMILIES = [
  { key: "step_recovery", label: "Step recoveries", swatch: "c1" },
  { key: "final_recovery", label: "Final recoveries", swatch: "c2" },
  { key: "auto_fix", label: "PR auto-fixes", swatch: "c3" },
  { key: "merge_conflict", label: "Merge conflicts", swatch: "c4" },
  { key: "failed_ci", label: "Failed CI", swatch: "c5" },
  { key: "cost", label: "Cost", swatch: "c6" },
  { key: "tokens", label: "Tokens", swatch: "c7" },
];
const COMPLEXITIES = [["", "All complexities"], ["low", "Low"], ["medium", "Medium"], ["hard", "Hard"], ["xhard", "Xhard"], ["unassessed", "Unassessed"]];
const MEASURES = [["count", "Count"], ["rate", "Per done task"]];
const PART_LABELS = {
  recovered: "recovered", failed: "failed", resume: "resumed", settled: "settled", escalated: "escalated", other: "other",
  failed_checks: "failed checks", conflict: "conflict", landing_review: "landing review",
  pr_head: "PR heads", sweep_filing: "sweep filings", invocations: "invocations",
};

const filters = { families: ["step_recovery", "final_recovery", "auto_fix", "merge_conflict", "failed_ci"], complexity: "", crew: "" };
/// Local view state.
export const frictionView = { measure: "count" };
let sequence = 0;

/// The endpoint query for `windowKey` under the current filters.
export function frictionQuery(windowKey) {
  const query = new URLSearchParams({ since: windowKey, families: filters.families.join(",") });
  if (filters.complexity) query.set("complexity", filters.complexity);
  if (filters.crew) query.set("crew", filters.crew);
  return `/api/diagnostics/delivery_friction?${query}`;
}

/// Fetch the trend for `windowKey`. A failed read, or a host whose Orbit
/// predates the endpoint, answers `{ unavailable }` so the other trends still
/// render. Every result carries a fence (generation, visit revision, window
/// and query) captured at request time, as the tokens trend does.
export function fetchFriction(windowKey) {
  const fence = { generation: ++sequence, revision: getWorkspaceRevision(), windowKey, query: frictionQuery(windowKey) };
  const query = fence.query;
  return fetchJson(query).then(
    (payload) => (payload && payload.window && Array.isArray(payload.families)
      ? { ...payload, query, frictionFence: fence }
      : { query, unavailable: "This host answered without a delivery-friction trend.", frictionFence: fence }),
    (error) => ({
      query,
      unavailable: error.status === 404 ? "This host's Orbit does not chart delivery friction yet. Upgrade it to see this trend." : error.message,
      frictionFence: fence,
    }),
  );
}

/// True only when `payload` is the newest friction request and its visit,
/// window and query still match current state.
export function isCurrentFrictionPayload(payload, windowKey) {
  const fence = payload && payload.frictionFence;
  if (!fence) return false;
  return fence.generation === sequence
    && fence.revision === getWorkspaceRevision()
    && fence.windowKey === windowKey
    && fence.query === frictionQuery(windowKey);
}

function compact(n) {
  const abs = Math.abs(n);
  if (abs >= 1e9) return `${(n / 1e9).toFixed(abs >= 1e10 ? 0 : 1)}B`;
  if (abs >= 1e6) return `${(n / 1e6).toFixed(abs >= 1e7 ? 0 : 1)}M`;
  if (abs >= 1e3) return `${(n / 1e3).toFixed(abs >= 1e4 ? 0 : 1)}k`;
  return Number.isInteger(n) ? String(n) : n.toFixed(abs >= 10 ? 1 : 2);
}

/// A family amount in its unit: "12", "$3.40", "1.2M tokens".
export function fmtAmount(unit, value) {
  if (value == null || !Number.isFinite(Number(value))) return "—";
  const n = Number(value);
  if (unit === "usd") return n >= 100 ? `$${Math.round(n).toLocaleString()}` : `$${n.toFixed(2)}`;
  return compact(n);
}

/// A family rate per done task: "0.42", "$1.10", "45k".
function fmtRate(unit, value) {
  if (value == null) return "—";
  if (unit === "usd") return `$${value.toFixed(2)}`;
  if (unit === "tokens") return compact(Math.round(value));
  return value >= 10 ? value.toFixed(1) : value.toFixed(2);
}

function unitWord(unit) {
  return unit === "usd" ? "USD" : unit === "tokens" ? "tokens" : "events";
}

function tasksDone(n) {
  return `${n} ${n === 1 ? "task" : "tasks"} done`;
}

function option(value, text, selected) {
  const node = el("option", { text });
  node.value = value;
  node.selected = selected;
  return node;
}

/// The filter row: the family multi-select, complexity and crew (each
/// refetches) and the count/rate measure (local).
export function frictionTools({ segGroup, windowKey, payload, rerender, onChange }) {
  const refetch = () => {
    // Clear to loading at once so the chart never shows the previous filter.
    onChange(null);
    fetchFriction(windowKey).then((next) => {
      if (isCurrentFrictionPayload(next, windowKey)) onChange(next);
    });
  };
  const picker = el("div", { class: "mx-seg mx-seg-wrap", role: "group", "aria-label": "Metric families" }, FAMILIES.map((family) => {
    const on = filters.families.includes(family.key);
    const button = el("button", { class: on ? "mx-seg-btn on" : "mx-seg-btn", type: "button", "aria-pressed": on ? "true" : "false", title: on ? `Remove ${family.label}` : `Add ${family.label}` }, [
      el("span", { class: `mx-swatch mx-s-${family.swatch}`, "aria-hidden": "true" }),
      family.label,
    ]);
    button.addEventListener("click", () => {
      const next = on ? filters.families.filter((key) => key !== family.key) : [...filters.families, family.key];
      // The chart always keeps one family.
      if (!next.length) return;
      filters.families = FAMILIES.map((entry) => entry.key).filter((key) => next.includes(key));
      refetch();
    });
    return button;
  }));
  const complexity = el("select", { class: "mx-select", "aria-label": "Task complexity", title: "Narrows done tasks, auto-fixes, failed CI, cost and tokens" },
    COMPLEXITIES.map(([value, text]) => option(value, text, value === filters.complexity)));
  complexity.addEventListener("change", () => {
    filters.complexity = complexity.value;
    refetch();
  });
  const crews = (payload && payload.available_crews) || [];
  const crewOptions = [["", "All crews"], ...crews.map((entry) => [entry.crew, `${entry.crew} (${entry.tasks})`])];
  if (filters.crew && !crews.some((entry) => entry.crew === filters.crew)) crewOptions.push([filters.crew, filters.crew]);
  const crew = el("select", { class: "mx-select", "aria-label": "Task crew", title: "Narrows done tasks, auto-fixes, failed CI, cost and tokens" },
    crewOptions.map(([value, text]) => option(value, text, value === filters.crew)));
  crew.addEventListener("change", () => {
    filters.crew = crew.value;
    refetch();
  });
  return [
    picker,
    segGroup("Measure", MEASURES, frictionView.measure, (key) => {
      if (key === frictionView.measure) return;
      frictionView.measure = key;
      rerender();
    }),
    complexity,
    crew,
  ];
}

/// "auto-fix 3 failed checks · 1 conflict".
function partsText(unit, breakdown) {
  const entries = Object.entries(breakdown || {}).filter(([, value]) => value);
  if (entries.length < 2) return "";
  return entries.map(([key, value]) => `${fmtAmount(unit, value)} ${PART_LABELS[key] || key}`).join(" · ");
}

/// A 1–2–5 axis from zero covering `max`, in about four steps.
function scaleOf(max) {
  const high = max > 0 ? max : 1;
  const raw = high * 1.08 / 4;
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 5, 10].map((m) => m * magnitude).find((candidate) => candidate >= raw);
  return { step, max: Math.ceil(high * 1.08 / step) * step };
}

/// The trend model metrics.js draws: cards, chart, table, or an empty state.
export function frictionModel(payload, { windowKey, shown, bucketTitle, bucketWidthLabel }) {
  const blurb = "Delivery friction in each bucket: step and final recoveries, PR auto-fixes, merge conflicts and failed CI as distinct events, cost in USD and normalized tokens. Per done task divides by the tasks that reached done in the same bucket, so throughput changes do not dominate.";
  if (!payload) return { blurb, cards: [], empty: { title: "Loading delivery friction…", detail: "", noWiden: true } };
  if (payload.unavailable) return { blurb, cards: [], empty: { title: "Delivery friction unavailable", detail: payload.unavailable, noWiden: true } };

  const rate = frictionView.measure === "rate";
  const seconds = payload.window.bucket_seconds;
  const starts = payload.bucket_starts || [];
  const minDone = payload.min_done;
  const isShown = (key) => shown[key] !== false;
  const meta = (key) => FAMILIES.find((family) => family.key === key) || { key, label: key, swatch: "crew-other" };
  const families = payload.families.map((family) => ({ ...family, ...meta(family.family), label: family.label || meta(family.family).label }));
  const filtering = Boolean(payload.complexity || payload.crew);
  const valueOf = (family, cell) => (rate ? cell.rate : cell.count);
  const fmtValue = (family, value) => (rate ? fmtRate(family.unit, value) : fmtAmount(family.unit, value));

  const cards = families.map((family) => {
    const total = family.total;
    const previous = family.previous;
    const value = valueOf(family, total);
    // Friction is worse when it rises, as the defects trend reads it.
    let deltaText = null;
    const prior = previous ? valueOf(family, previous) : null;
    if (prior != null && value != null && prior > 0) {
      const change = (value - prior) / prior * 100;
      const abs = Math.abs(change);
      deltaText = {
        cls: change > 5 ? "bad" : change < -5 ? "good" : "flat",
        text: `${change >= 0 ? "▲" : "▼"} ${abs >= 10 ? Math.round(abs) : abs.toFixed(1)}% vs prev ${windowKey}`,
      };
    }
    return {
      key: family.key,
      swatch: family.swatch,
      label: family.label,
      tag: filtering && !family.filtered ? "unfiltered" : "",
      on: isShown(family.key),
      value: fmtValue(family, value),
      delta: deltaText,
      sub: rate ? `${fmtAmount(family.unit, total.count)} ${unitWord(family.unit)}` : `${fmtRate(family.unit, total.rate)} per done task`,
      foot: tasksDone(total.denominator),
    };
  });

  const visible = families.filter((family) => isShown(family.key));
  const anyValue = families.some((family) => family.total.count > 0);
  if (!anyValue && !(payload.done && payload.done.total)) {
    return {
      blurb,
      cards,
      empty: { title: `No delivery friction or done tasks in the last ${windowKey === "all" ? "recorded history" : windowKey}`, detail: "" },
    };
  }

  // One axis when every drawn family shares a unit; otherwise each line is
  // scaled to its own peak and the tooltip carries the values.
  const units = new Set(visible.map((family) => family.unit));
  const shared = units.size <= 1;
  const peak = (family) => Math.max(0, ...family.points.map((point) => valueOf(family, point) || 0));
  const sharedMax = Math.max(0, ...visible.map(peak));
  const scale = scaleOf(shared ? sharedMax : 100);
  const y = (value) => 100 - Math.min(Math.max(value, 0), scale.max) / scale.max * 100;
  const yTicks = [];
  const unit = shared ? [...units][0] || "events" : null;
  for (let v = 0; v <= scale.max; v += scale.step) {
    yTicks.push({ top: y(v), label: shared ? fmtValue({ unit }, v) : `${v}%` });
  }

  const notes = [
    `${bucketWidthLabel(seconds)}, aligned to the clock`,
    rate ? "per task done in the same bucket" : "distinct occurrences; USD and tokens summed",
  ];
  if (!shared) notes.push("mixed units: each line is scaled to its own peak (axis = % of peak)");
  if (filtering) notes.push("complexity and crew narrow done tasks, auto-fixes, failed CI, cost and tokens; recoveries and conflicts stay unfiltered");

  return {
    blurb,
    cards,
    chart: {
      kind: "friction",
      buckets: starts.map((start) => ({ start })),
      seconds,
      yTicks,
      lines: visible.map((family) => {
        const top = shared ? 1 : peak(family);
        return {
          key: family.swatch,
          joined: rate,
          points: family.points.map((point, i) => {
            const value = valueOf(family, point);
            if (value == null) return null;
            const drawn = shared ? value : (top > 0 ? value / top * 100 : 0);
            return { i, y: y(drawn), hollow: point.sparse };
          }),
        };
      }),
      notes,
      hollowText: `hollow = sparse bucket: fewer than ${minDone} tasks done`,
      aria: `Delivery friction, ${rate ? "per done task" : "counts"}, window ${windowKey}: ${visible.map((family) => `${family.label} ${fmtValue(family, valueOf(family, family.total))}`).join(", ")}. Use the Table view for per-bucket values.`,
      tip: (index) => {
        const tip = { title: bucketTitle(starts[index], seconds, index === starts.length - 1), rows: [], foot: "count · tasks done · rate per done task" };
        let sparse = false;
        for (const family of families) {
          const point = family.points[index];
          sparse = sparse || point.sparse;
          tip.rows.push({
            key: family.swatch,
            name: family.label,
            value: `${fmtAmount(family.unit, point.count)} · ${point.denominator} done · ${fmtRate(family.unit, point.rate)}/task`,
          });
        }
        const parts = families
          .map((family) => {
            const text = partsText(family.unit, family.points[index].breakdown);
            return text ? `${family.label}: ${text}` : "";
          })
          .filter(Boolean);
        if (parts.length) tip.extra = parts.join(" · ");
        if (sparse) tip.warn = `Sparse bucket: fewer than ${minDone} tasks done, so rates swing on a few tasks.`;
        return tip;
      },
    },
    table: {
      caption: `Delivery friction, count (rate per done task), tasks done · ${bucketWidthLabel(seconds)} · newest first · * = sparse bucket`,
      heads: [...families.map((family) => family.label), "Tasks done"],
      rows: starts.map((start, i) => {
        const done = payload.done ? payload.done.points[i] : 0;
        const sparse = families.some((family) => family.points[i].sparse);
        return {
          title: `${bucketTitle(start, seconds, i === starts.length - 1)}${sparse ? " *" : ""}`,
          cells: [
            ...families.map((family) => {
              const point = family.points[i];
              return point.count ? `${fmtAmount(family.unit, point.count)} (${fmtRate(family.unit, point.rate)})` : null;
            }),
            String(done),
          ],
        };
      }).reverse(),
    },
  };
}

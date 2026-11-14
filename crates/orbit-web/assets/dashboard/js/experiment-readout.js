// Orbit dashboard Analytics › Experiments: the readout's arm cards, forest plot,
// cumulative series, per-complexity table, crew mix and per-arm evidence, with
// the formatting and card builders the rest of the page shares. Pure rendering
// from the readout payload: every node is built with DOM calls.

import { el, formatDate, formatDateTime, formatClock, pinnedInfoHint, detailsPanel } from './common.js';
import { svg, linePath } from './sparkline.js';

export { el };

// Arms take the shared categorical palette in definition order, so an arm
// keeps its color on every chart of the page; past seven they fold into one.
const ARM_COLORS = 7;
const GUARDRAIL_LABELS = { ok: "ok", breached: "breached", straddles: "straddles", unknown: "unknown" };
// ---- formatting -------------------------------------------------------------

export function num(value) {
  const n = Number(value);
  return value == null || !Number.isFinite(n) ? null : n;
}

function trim(n, digits) {
  return Number(n.toFixed(digits)).toString();
}

function compact(n) {
  const abs = Math.abs(n);
  if (abs >= 1e9) return `${trim(n / 1e9, 1)}B`;
  if (abs >= 1e6) return `${trim(n / 1e6, 1)}M`;
  if (abs >= 1e3) return `${trim(n / 1e3, abs >= 1e4 ? 0 : 1)}k`;
  if (abs > 0 && abs < 0.1) return Number(n.toPrecision(2)).toString();
  return trim(n, abs >= 100 ? 0 : abs >= 10 ? 1 : 2);
}

/// A metric value in its unit; `diff` prints a signed difference.
export function fmtValue(value, unit, { diff = false } = {}) {
  const n = num(value);
  if (n == null) return "—";
  const sign = diff && n > 0 ? "+" : diff && n < 0 ? "−" : "";
  const abs = diff ? Math.abs(n) : n;
  switch (unit) {
    case "rate": return diff ? `${sign}${trim(abs * 100, 1)}pt` : `${trim(abs * 100, abs >= 0.1 ? 0 : 1)}%`;
    case "usd": return `${sign}$${abs >= 100 ? Math.round(abs).toLocaleString() : abs.toFixed(2)}`;
    case "hours": return abs >= 48 ? `${sign}${trim(abs / 24, 1)}d` : `${sign}${trim(abs, 1)}h`;
    case "minutes": return abs >= 120 ? `${sign}${trim(abs / 60, 1)}h` : `${sign}${trim(abs, 1)}m`;
    case "normalized_tokens": return `${sign}${compact(abs)}`;
    default: return `${sign}${compact(abs)}`;
  }
}

function fmtInterval(interval, unit, opts) {
  if (!Array.isArray(interval) || interval.length !== 2) return null;
  return `${fmtValue(interval[0], unit, opts)} to ${fmtValue(interval[1], unit, opts)}`;
}

export function metricLabel(name) {
  return String(name || "").replace(/_/g, " ");
}

function unitLabel(unit) {
  return { normalized_tokens: "normalized tokens", usd: "USD" }[unit] || unit;
}

export function pluralCount(n, word) {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

export function armColor(index) {
  return index < ARM_COLORS ? `mx-s-c${index + 1}` : "mx-s-crew-other";
}

export function armIndex(readout) {
  const index = new Map();
  (readout.arms || []).forEach((arm, i) => index.set(arm.name, i));
  return (name) => index.get(name) ?? ARM_COLORS;
}

export function armSwatch(className, extra = "") {
  return el("span", { class: `mx-swatch ${className}${extra}`, "aria-hidden": "true" });
}

export function experimentBadge(text, className) {
  return el("span", { class: `ex-badge ${className}`, text });
}

export function experimentCard(title, children, { meta = null, className = "", hint = null } = {}) {
  return el("section", { class: `mx-card ex-card ${className}`.trim(), "aria-label": title }, [
    el("div", { class: "mx-card-head" }, [
      el("h2", { class: "mx-card-title" }, [title, hint ? pinnedInfoHint(hint) : null]),
      meta ? el("span", { class: "mx-card-meta", text: meta }) : null,
    ]),
    ...children,
  ]);
}

export function experimentEmpty(title, detail) {
  return el("div", { class: "mx-empty ex-empty" }, [
    el("div", { class: "mx-empty-title", text: title }),
    detail ? el("div", { class: "mx-empty-detail", text: detail }) : null,
  ]);
}

// A detail section with nothing to show yet says why behind its icon.
export function experimentEmptyWithReason(title, reason) {
  return el("div", { class: "mx-empty ex-empty" }, [
    el("div", { class: "mx-empty-title" }, [title, pinnedInfoHint(reason)]),
  ]);
}

// The operator's conclusion names the winner, even when the readout withholds
// its own verdict (a sample-ratio mismatch); otherwise the readout's verdict.
function armVerdict(arm, verdict, concludedArm) {
  if (concludedArm) return concludedArm === arm.name ? "winner (concluded)" : arm.control ? "control" : "not chosen";
  if (arm.control) return "control";
  const state = verdict?.state || "collecting";
  if (state === "winner") return verdict.arm === arm.name ? "winner" : "not the winner";
  return verdict?.label || state;
}

function deliveryIsLow(delivery, readout) {
  return delivery?.rate != null && delivery.rate < (readout.experiment?.min_delivered_rate ?? 0);
}

export function armCards(experiment, readout, primary, color) {
  const arms = readout.arms || [];
  const guardrails = readout.guardrails || [];
  const unit = primary?.unit;
  const cards = arms.map((arm) => {
    const estimate = primary?.arms?.find((row) => row.arm === arm.name);
    const difference = primary?.differences?.find((row) => row.arm === arm.name);
    const interval = fmtInterval(estimate?.mean_interval, unit);
    const state = armVerdict(arm, readout.verdict, experiment.concluded_arm);
    const rails = guardrails.map((rail) => {
      const row = rail.arms?.find((entry) => entry.arm === arm.name);
      if (!row) return null;
      return el("li", { class: `ex-rail ex-rail-${row.status}` }, [
        el("span", { text: `${metricLabel(rail.metric)} ${rail.threshold}` }),
        el("span", { class: "ex-rail-status", text: GUARDRAIL_LABELS[row.status] || row.status }),
      ]);
    }).filter(Boolean);
    const estimateDifference = num(difference?.overall?.estimate);
    const verdict = forestVerdict(orientedInterval(difference?.overall?.interval, primary?.better === "lower" ? -1 : 1));
    return el("div", { class: `mx-sc ex-arm${state.startsWith("winner") ? " ex-arm-winner" : ""}`, role: "group", "aria-label": `Arm ${arm.name}` }, [
      el("span", { class: "mx-sc-label" }, [
        armSwatch(armColor(color(arm.name))),
        el("span", { class: "ex-arm-title", text: arm.name }),
        el("span", { class: "mx-sc-tag", text: `weight ${arm.weight}` }),
        arm.drifted ? el("span", { class: "mx-sc-tag ex-warn-text", text: "drifted" }) : null,
        deliveryIsLow(arm.delivery, readout) ? el("span", { class: "mx-sc-tag ex-warn-text", text: "low delivery" }) : null,
        el("span", { class: `ex-arm-verdict ex-arm-verdict-${state.replace(/\W+/g, "-").replace(/-$/, "")}`, text: state }),
      ]),
      el("span", { class: "ex-arm-figure" }, [
        el("span", { class: "mx-sc-value", text: estimate ? fmtValue(estimate.mean, unit) : "—" }),
        estimateDifference != null ? el("span", {
          class: `ex-arm-diff ex-forest-${verdict}`,
          text: `${fmtValue(estimateDifference, unit, { diff: true })} vs control`,
          title: FOREST_VERDICTS[verdict],
        }) : null,
      ]),
      el("span", { class: "mx-sc-foot", text: interval ? `95% ${interval}` : "no interval yet" }),
      el("span", { class: "mx-sc-sub ex-arm-n" }, [
        `n ${arm.n}`,
        estimate ? ` · ${estimate.n} settled` : "",
        estimate?.open ? ` · ${estimate.open} open` : "",
      ]),
      rails.length ? el("ul", { class: "ex-rails", "aria-label": "Guardrails" }, rails) : null,
    ]);
  });
  const title = primary ? `Arms · ${metricLabel(primary.metric)}` : "Arms";
  const meta = primary ? `${primary.kind === "rate" ? "rate" : "mean"}, ${primary.better} is better · ${unitLabel(primary.unit)}` : "no primary metric";
  return experimentCard(title, [el("div", { class: "mx-series ex-arms" }, cards)], { meta });
}

// One row per arm of what sits behind its card: delivery,
// the per-protocol estimate and cost coverage.
export function armEvidenceCard(readout) {
  const arms = readout.arms || [];
  const primaryName = readout.experiment?.primary_metric;
  const unit = (readout.metrics || []).find((metric) => metric.metric === primaryName)?.unit;
  const cost = (readout.metrics || []).find((metric) => metric.metric === "cost_usd");
  const costOf = (arm) => cost?.arms?.find((row) => row.arm === arm.name)?.cost_coverage;
  const deliveryRoles = arms.find((arm) => arm.delivery)?.delivery.roles || [];
  const minDelivered = Math.round((readout.experiment?.min_delivered_rate ?? 0) * 100);
  const columns = [
    arms.some((arm) => arm.delivery) ? {
      head: "Delivered",
      hint: `Settled tasks whose ${deliveryRoles.join(", ")} step received this arm's overlay and finished successfully. Below ${minDelivered}% the verdict is withheld. Pending tasks are in flight and not yet delivered.`,
      cell: (arm) => (arm.delivery ? {
        text: `${arm.delivery.delivered} of ${arm.delivery.assigned - arm.delivery.pending}`
          + (arm.delivery.rate != null ? ` (${Math.round(arm.delivery.rate * 100)}%)` : "")
          + (arm.delivery.pending ? ` · ${arm.delivery.pending} pending` : ""),
        className: deliveryIsLow(arm.delivery, readout) ? "ex-warn-text" : null,
      } : { text: "—" }),
    } : null,
    readout.per_protocol ? {
      head: "Per-protocol",
      hint: "The primary metric over delivered runs only: each checked treatment arm keeps its delivered tasks, other arms all theirs. The arm card's value is intention-to-treat, over every assigned run.",
      cell: (arm) => {
        const row = readout.per_protocol.metric?.arms?.find((entry) => entry.arm === arm.name);
        return { text: `${row?.n ? fmtValue(row.mean, unit) : "—"} · n ${row?.n ?? 0}` };
      },
    } : null,
    arms.some(costOf) ? {
      head: "Cost",
      hint: "Settled tasks: reported means every invocation has a provider cost; estimated uses token prices for at least one invocation. Tasks missing any cost are unmeasured and excluded from cost comparisons. Open tasks are separate.",
      cell: (arm) => {
        const coverage = costOf(arm);
        return {
          text: coverage ? `${coverage.reported} reported · ${coverage.estimated} estimated · ${coverage.unmeasured} unmeasured` : "—",
          className: "ex-arm-cost",
        };
      },
    } : null,
  ].filter(Boolean);
  const table = el("table", { class: "mx-table ex-table" }, [
    el("thead", {}, [el("tr", {}, [
      el("th", { scope: "col", text: "Arm" }),
      ...columns.map((column) => el("th", { scope: "col" }, [column.head, pinnedInfoHint(column.hint)])),
    ])]),
    el("tbody", {}, arms.map((arm, index) => el("tr", {}, [
      el("th", { scope: "row" }, [el("span", { class: "ex-arm-name" }, [armSwatch(armColor(index)), arm.name])]),
      ...columns.map((column) => {
        const cell = column.cell(arm);
        return el("td", { class: cell.className || null, text: cell.text });
      }),
    ]))),
  ]);
  return experimentCard("Arm evidence", [el("div", { class: "mx-table-wrap", tabindex: 0, role: "region", "aria-label": "Arm evidence" }, [table])], { className: "mx-wide" });
}

// A forest plot: one row per metric and treatment arm, the dot the
// difference from control and the bar its 95% interval. Metrics have
// different units, so each metric's rows share their own scale, symmetric
// around zero, and print their numbers; no axis spans two metrics.
// Every axis points the same way: better is to the right. A lower-is-better
// metric is drawn mirrored, while its printed numbers stay the signed arm
// minus control. The dot and bar take the verdict's colour, not the arm's.
const FOREST_VERDICTS = {
  better: "better at 95%",
  worse: "worse at 95%",
  unclear: "interval includes zero",
};

// The interval with better as positive, or null without two finite bounds.
function orientedInterval(interval, sign) {
  const bounds = Array.isArray(interval) ? interval.map(num) : null;
  return bounds && bounds.length === 2 && bounds.every((n) => n != null)
    ? bounds.map((n) => sign * n).sort((a, b) => a - b) : null;
}

// `oriented` is the interval with better as positive.
function forestVerdict(oriented) {
  if (!oriented) return "unclear";
  if (oriented[0] > 0) return "better";
  if (oriented[1] < 0) return "worse";
  return "unclear";
}

// The rows the call rests on, primary and guardrails, stay on the page; every
// other metric is one click away in a section whose open state survives a poll.
export function forestCard(experiment, readout, color) {
  const metrics = (readout.metrics || []).filter((metric) => metric.measured
    && (metric.differences || []).some((diff) => num(diff.overall?.estimate) != null));
  const primaryName = readout.experiment?.primary_metric;
  const guardNames = new Set((readout.guardrails || []).map((rail) => rail.metric));
  metrics.sort((a, b) => (b.metric === primaryName) - (a.metric === primaryName)
    || guardNames.has(b.metric) - guardNames.has(a.metric));
  const unmeasured = (readout.metrics || []).filter((metric) => !metric.measured).map((metric) => `${metricLabel(metric.metric)}${metric.exploratory ? " (exploratory)" : ""}`);
  if (!metrics.length) {
    return experimentCard("Difference from control", [experimentEmptyWithReason("No differences yet", "Every metric needs settled tasks in control and a treatment arm.")]);
  }
  const forestGroup = (metric) => {
    const bounds = metric.differences.flatMap((diff) => [diff.overall?.estimate, ...(diff.overall?.interval || [])])
      .map(num).filter((n) => n != null).map(Math.abs);
    const extent = Math.max(...bounds, 0) * 1.1 || 1;
    const sign = metric.better === "lower" ? -1 : 1;
    const x = (value) => `${(((sign * value + extent) / (2 * extent)) * 100).toFixed(2)}%`;
    const direction = metric.better === "lower" ? "lower is better" : "higher is better";
    const tags = [
      metric.metric === primaryName ? "primary" : null,
      metric.exploratory ? "exploratory" : null,
      guardNames.has(metric.metric) ? "guardrail" : null,
    ].filter(Boolean);
    const rows = metric.differences.map((diff) => {
      const estimate = num(diff.overall?.estimate);
      const interval = diff.overall?.interval;
      const colorClass = armColor(color(diff.arm));
      const text = estimate == null ? "—" : fmtValue(estimate, metric.unit, { diff: true });
      const range = fmtInterval(interval, metric.unit, { diff: true });
      const oriented = orientedInterval(interval, sign);
      const verdict = forestVerdict(oriented);
      const plot = el("span", { class: "ex-forest-plot", "aria-hidden": "true" }, [
        el("span", { class: "ex-forest-zero", style: { left: "50%" } }),
        oriented ? el("span", {
          class: `ex-forest-ci ex-forest-${verdict}`,
          style: { left: x(sign * oriented[0]), width: `${(((oriented[1] - oriented[0]) / (2 * extent)) * 100).toFixed(2)}%` },
        }) : null,
        estimate != null ? el("span", { class: `ex-forest-dot ex-forest-${verdict}`, style: { left: x(estimate) } }) : null,
      ]);
      return el("div", {
        class: "ex-forest-row",
        role: "listitem",
        title: `${metricLabel(metric.metric)} (${direction}), ${diff.arm} minus control: ${text}${range ? ` (95% ${range})` : ""}; ${FOREST_VERDICTS[verdict]}`,
      }, [
        el("span", { class: "ex-forest-arm" }, [armSwatch(colorClass), el("span", { text: diff.arm })]),
        plot,
        el("span", { class: "ex-forest-value" }, [
          el("span", { class: "ex-forest-est", text }),
          range ? el("span", { class: "ex-forest-range", text: range }) : null,
        ]),
      ]);
    });
    return el("div", { class: "ex-forest-group" }, [
      el("div", { class: "ex-forest-metric" }, [
        el("span", { class: "ex-forest-name", text: metricLabel(metric.metric) }),
        ...tags.map((tag) => el("span", { class: "mx-sc-tag", text: tag })),
      ]),
      el("div", { class: "ex-forest-rows", role: "list" }, rows),
    ]);
  };
  const forestAxis = () => el("div", { class: "ex-forest-axis", "aria-hidden": "true" }, [
    el("span"),
    el("span", { class: "ex-forest-axis-plot" }, [
      el("span", { text: "← worse" }),
      el("span", { class: "ex-forest-axis-better", text: "better →" }),
    ]),
    el("span"),
  ]);
  const decides = (metric) => metric.metric === primaryName || guardNames.has(metric.metric);
  const deciding = metrics.filter(decides);
  const others = metrics.filter((metric) => !decides(metric));
  let otherMetrics = null;
  if (others.length || unmeasured.length) {
    otherMetrics = detailsPanel(`experiment-other-metrics:${experiment.id}`, { class: "ex-forest-others" });
    otherMetrics.append(
      el("summary", { class: "mx-muted", text: `Other metrics (${others.length})` }),
      el("div", { class: "ex-forest" }, [
        others.length ? forestAxis() : null,
        ...others.map(forestGroup),
        unmeasured.length ? el("p", { class: "ex-note", text: `Not measured yet: ${unmeasured.join(", ")}.` }) : null,
      ]),
    );
  }
  return experimentCard("Difference from control", [
    el("div", { class: "ex-forest" }, deciding.length
      ? [forestAxis(), ...deciding.map(forestGroup)]
      : [experimentEmptyWithReason("No primary or guardrail difference yet", "Each needs settled tasks in control and a treatment arm.")]),
    otherMetrics,
  ], {
    hint: "Arm minus control, post-stratified by complexity, with 95% intervals. Each metric has its own scale around zero, and better is always to the right.\nLower-is-better metrics such as cost and tokens are drawn mirrored so that better points right; their numbers stay the signed arm minus control, so a cost cut prints negative.\nGreen is better and red worse at 95%; grey is a bar that crosses the zero line, a difference the sample cannot tell from none.\nThe 95% intervals are post-stratified bootstrap intervals of the difference for mean and rate metrics alike. Wilson intervals apply to a single arm's rate estimate.",
  });
}

// The primary metric's per-arm cumulative series at each bucket end of the
// shared trend ladder, over settled tasks assigned by then: the cumulative
// mean (a rate for a 0/1 metric), or for a per-line primary the ratio of
// accumulated numerators over accumulated reviewable lines.

// Axis text follows the dashboard clock at the bucket's own precision. An
// hour or 6 hours can fall on the same clock time on a later day, so those
// ticks keep the date. A day or wider is a calendar date.
function cumulativeTick(start, seconds) {
  if (!start) return "—";
  const width = Number(seconds);
  if (Number.isFinite(width) && width >= 86400) return formatDate(start);
  if (Number.isFinite(width) && width >= 3600) {
    return `${formatDate(start)} ${formatClock(start, { seconds: false, zone: false })}`;
  }
  if (Number.isFinite(width) && width > 0) return formatClock(start, { seconds: false, zone: false });
  return formatDateTime(start);
}

// Assignments in a finished bucket are those before the next boundary. The
// last bucket is still open: its cumulative values stop at `window.until`
// when that cutoff is sooner than the boundary.
function cumulativeEffectiveEnd(start, seconds, until, last) {
  const opened = new Date(start).getTime();
  if (!Number.isFinite(opened)) return null;
  const width = Number(seconds);
  const boundary = Number.isFinite(width) && width > 0 ? opened + width * 1000 : null;
  if (!last) return boundary;
  const cutoff = until == null || until === "" ? NaN : new Date(until).getTime();
  if (!Number.isFinite(cutoff)) return boundary;
  return boundary == null ? cutoff : Math.min(boundary, cutoff);
}

function cumulativeHover(start, seconds, until, last) {
  const endMs = cumulativeEffectiveEnd(start, seconds, until, last);
  if (!start || endMs == null) return formatDateTime(start);
  const end = new Date(endMs);
  const withSeconds = end.getUTCSeconds() !== 0 || end.getUTCMilliseconds() !== 0;
  const startText = formatDateTime(start);
  const endText = formatDate(start) === formatDate(end)
    ? formatClock(end, { seconds: withSeconds, zone: false })
    : formatDateTime(end, { seconds: withSeconds });
  return `${startText}–${endText}`;
}

export function cumulativeCard(readout, primary, color) {
  const series = readout.cumulative;
  const unit = primary?.unit;
  const title = "Cumulative primary metric";
  if (!primary || !series || !(series.arms || []).length) {
    return experimentCard(title, [experimentEmptyWithReason("No primary metric", "This experiment names no primary metric to chart.")], { className: "mx-narrow" });
  }
  const arms = series.arms;
  const buckets = Math.max(...arms.map((arm) => (arm.points || []).length), 0);
  const values = arms.flatMap((arm) => (arm.points || []).map((point) => num(point.value))).filter((n) => n != null);
  if (!values.length || buckets === 0) {
    return experimentCard(title, [experimentEmptyWithReason("Nothing settled yet", "The series starts when an enrolled task's primary outcome settles.")], { meta: metricLabel(primary.metric), className: "mx-narrow" });
  }
  let lo = Math.min(...values);
  let hi = Math.max(...values);
  if (unit === "rate") { lo = 0; hi = Math.max(hi, 1e-9) > 0.5 ? 1 : Math.min(1, hi * 1.2); }
  else {
    const pad = (hi - lo) * 0.1 || Math.abs(hi) * 0.1 || 1;
    lo = Math.max(0, lo - pad);
    hi += pad;
  }
  const xOf = (index) => (buckets === 1 ? 50 : (index / (buckets - 1)) * 100);
  const yOf = (value) => 100 - ((value - lo) / (hi - lo || 1)) * 100;
  const plot = svg("svg", { viewBox: "0 0 1000 1000", preserveAspectRatio: "none", class: "ex-cum-lines", "aria-hidden": "true" });
  const ends = [];
  arms.forEach((arm) => {
    const colorClass = armColor(color(arm.arm));
    const points = (arm.points || []).map((point, index) => {
      const value = num(point.value);
      return value == null ? null : { x: xOf(index), y: yOf(value) };
    });
    plot.appendChild(svg("path", { d: linePath(points), class: `mx-line ${colorClass}`, "vector-effect": "non-scaling-stroke" }));
    const last = [...points].reverse().find(Boolean);
    if (last) ends.push(el("span", { class: `mx-pt ${colorClass}`, style: { left: `${last.x}%`, top: `${last.y}%` } }));
  });
  const ticks = [hi, (hi + lo) / 2, lo].map((value) =>
    el("span", { class: "ex-cum-ytick", style: { top: `${yOf(value)}%` }, text: fmtValue(value, unit) }));
  const firstPoints = arms[0].points || [];
  const bucketSeconds = series.window?.bucket_seconds;
  const bucketUntil = series.window?.until;
  const xTicks = [0, Math.floor((buckets - 1) / 2), buckets - 1]
    .filter((index, i, all) => all.indexOf(index) === i)
    .map((index) => el("span", {
      class: "ex-cum-xtick",
      style: { left: `${xOf(index)}%` },
      text: cumulativeTick(firstPoints[index]?.start, bucketSeconds),
    }));
  const tip = el("div", { class: "mx-tip ex-cum-tip" });
  tip.hidden = true;
  const cursor = el("div", { class: "mx-cursor" });
  cursor.hidden = true;
  const hover = el("div", { class: "ex-cum-hover" });
  for (let index = 0; index < buckets; index += 1) {
    const column = el("span", { class: "ex-cum-col" });
    const show = () => {
      tip.replaceChildren(
        el("div", { class: "mx-tip-title", text: cumulativeHover(firstPoints[index]?.start, bucketSeconds, bucketUntil, index === buckets - 1) }),
        ...arms.map((arm) => {
          const point = arm.points?.[index] || {};
          return el("div", { class: "mx-tip-row" }, [
            el("span", { class: "mx-tip-name" }, [armSwatch(armColor(color(arm.arm))), el("span", { text: arm.arm })]),
            el("span", { class: "mx-tip-n", text: `n ${point.n ?? 0}` }),
            el("span", { class: "mx-tip-value", text: fmtValue(point.value, unit) }),
          ]);
        }),
      );
      const left = xOf(index);
      cursor.style.left = `${left}%`;
      tip.hidden = false;
      // This narrow card can sit against the pane's clipped right edge even
      // at its middle bucket. Measure the shown tip, then keep it inside both
      // the plot and the viewport, including when its CSS width changes.
      const bounds = hover.getBoundingClientRect();
      const minLeft = Math.max(bounds.left, 0);
      const maxRight = Math.min(bounds.right, document.documentElement.clientWidth);
      tip.style.maxWidth = `${Math.max(0, maxRight - minLeft)}px`;
      const tipWidth = tip.getBoundingClientRect().width;
      const anchor = bounds.left + bounds.width * left / 100;
      const preferred = anchor + 14 + tipWidth <= maxRight ? anchor + 14 : anchor - 14 - tipWidth;
      tip.style.left = `${Math.max(minLeft, Math.min(preferred, maxRight - tipWidth)) - bounds.left}px`;
      cursor.hidden = false;
    };
    column.addEventListener("mouseenter", show);
    hover.appendChild(column);
  }
  hover.addEventListener("mouseleave", () => { tip.hidden = true; cursor.hidden = true; });
  const legend = el("div", { class: "ex-legend" }, arms.map((arm) =>
    el("span", { class: "ex-legend-item" }, [armSwatch(armColor(color(arm.arm))), arm.arm])));
  const width = series.window?.bucket_seconds;
  const bucketText = width ? (width % 604800 === 0 ? `${width / 604800}w` : width >= 86400 ? `${width / 86400}d` : width >= 3600 ? `${width / 3600}h` : `${width / 60}m`) : "?";
  const label = `${metricLabel(primary.metric)} cumulative per arm: ${arms.map((arm) => {
    const last = [...(arm.points || [])].reverse().find((point) => num(point.value) != null);
    return `${arm.arm} ${last ? fmtValue(last.value, unit) : "no value"}`;
  }).join(", ")}`;
  return experimentCard(title, [
    legend,
    el("div", { class: "ex-cum", role: "img", "aria-label": label }, [
      el("div", { class: "ex-cum-y" }, ticks),
      el("div", { class: "ex-cum-frame" }, [plot, ...ends, cursor, hover, tip]),
    ]),
    el("div", { class: "ex-cum-x" }, xTicks),
  ], {
    meta: unitLabel(unit),
    className: "mx-narrow",
    hint: `${metricLabel(primary.metric)} over settled tasks assigned by each ${bucketText} bucket's end.`,
  });
}

export function complexityCard(primary, color) {
  const title = "By complexity";
  const rows = [];
  for (const diff of primary?.differences || []) {
    for (const cell of diff.by_complexity || []) rows.push({ arm: diff.arm, ...cell });
  }
  if (!rows.length) {
    return experimentCard(title, [experimentEmptyWithReason("No complexity cells yet", "A cell needs settled tasks in both the arm and control.")], { className: "mx-wide" });
  }
  const unit = primary.unit;
  const table = el("table", { class: "mx-table ex-table" }, [
    el("caption", { class: "visually-hidden", text: `${metricLabel(primary.metric)}: arm minus control within each complexity` }),
    el("thead", {}, [el("tr", {}, [
      el("th", { scope: "col", text: "Complexity" }),
      el("th", { scope: "col", text: "Arm" }),
      el("th", { scope: "col", class: "num", text: "n arm" }),
      el("th", { scope: "col", class: "num", text: "n control" }),
      el("th", { scope: "col", class: "num", text: "Difference" }),
      el("th", { scope: "col", class: "num", text: "95% interval" }),
    ])]),
    el("tbody", {}, rows.map((row) => el("tr", {}, [
      el("th", { scope: "row", text: row.complexity }),
      el("td", {}, [el("span", { class: "ex-arm-name" }, [armSwatch(armColor(color(row.arm))), row.arm])]),
      el("td", { class: "num", text: String(row.n_arm ?? 0) }),
      el("td", { class: "num", text: String(row.n_control ?? 0) }),
      el("td", { class: "num", text: fmtValue(row.estimate, unit, { diff: true }) }),
      el("td", { class: "num", text: fmtInterval(row.interval, unit, { diff: true }) || "—" }),
    ]))),
  ]);
  return experimentCard(title, [el("div", { class: "mx-table-wrap", tabindex: 0, role: "region", "aria-label": "Difference by complexity" }, [table])], {
    meta: `${metricLabel(primary.metric)} · arm minus control`,
    className: "mx-wide",
  });
}

// The invocations behind an arm's crew mix, which can run to dozens of rows:
// a collapsed list whose open state survives a poll.
function recordedEffort(arm, experimentId) {
  const rows = arm.effort_breakdown || [];
  if (!rows.length) return el("span", { class: "mx-muted ex-effort", text: "Recorded effort: unknown" });
  const panel = detailsPanel(`experiment-effort:${experimentId || "unknown"}:${arm.name}`, { class: "ex-effort" });
  panel.append(
    el("summary", { class: "mx-muted", text: "Recorded effort" }),
    el("ul", { class: "ex-effort-rows" }, rows.map((item) => el("li", {
      text: `${item.crew || "unknown crew"} · ${item.model || "unknown model"} · ${item.reasoning_effort === null ? "provider default" : item.reasoning_effort || "unknown"}: ${item.invocations} invocations`,
    }))),
  );
  return panel;
}

export function crewCard(readout, color, experimentId = readout?.experiment?.id) {
  const arms = readout.arms || [];
  const rows = arms.map((arm) => {
    const total = (arm.crew_mix || []).reduce((sum, row) => sum + (row.tasks || 0), 0);
    const crews = (arm.crew_mix || []).map((row) =>
      el("span", { class: "ex-crew" }, [
        el("span", { class: "ex-crew-name", text: row.crew }),
        el("span", { class: "ex-crew-n", text: `${row.tasks} · ${total ? Math.round((row.tasks / total) * 100) : 0}%` }),
      ]));
    return el("div", { class: "ex-crew-row" }, [
      el("span", { class: "ex-arm-name" }, [armSwatch(armColor(color(arm.name))), arm.name]),
      el("span", { class: "ex-crews" }, crews.length ? crews : [el("span", { class: "mx-muted", text: "no tasks" })]),
      recordedEffort(arm, experimentId),
    ]);
  });
  return experimentCard("Crew mix", [el("div", { class: "ex-crew-rows" }, rows)], {
    className: "mx-narrow",
    hint: "Tasks without an explicit crew draw from the arm's configured pool: its overlay pool when set, otherwise the ordinary pool. Different pools can produce different mixes by design; within a pool, weighted draws balance only in expectation.",
  });
}

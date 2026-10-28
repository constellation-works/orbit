import { backlogForecastSummary, drainBacklogForecastLink, fetchBacklogForecast } from './forecast.js';
import { getWorkspaceRevision, isAggregateView, el } from './common.js';
import { sparkline, svg } from './sparkline.js';
import { $, autoDrainNumber, operationsState, registerOperationsReset, selectedWorkspace, taskLink, time } from './operations-shared.js';

// The backlog forecast summary under the admission block. Keyed by workspace
// revision so a late response cannot paint another workspace. Pending omits
// the node; a settled miss is one muted line.
let backlogForecastCache = { revision: null, status: "pending", summary: null };
let backlogForecastFlight = null;

function asForecast(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : null;
}

function forecastBuiltAt(forecast) {
  return typeof forecast?.forecast_at === "string" && forecast.forecast_at ? forecast.forecast_at : "";
}

// `detail` and `stall.friction_id` are not rendered: a persisted starvation
// sentence names a task id, and a friction id is not the hold itself.
function forecastHoldDetail(item) {
  const percent = Number(item?.percent);
  const minute = Number(item?.minute);
  if (item?.resource && Number.isFinite(percent) && Number.isFinite(minute)) {
    return `${item.resource} ${percent.toFixed(0)}% at +${minute}m`;
  }
  return "waiting for the protected queue head to fit";
}

function forecastAmount(resource, value) {
  const amount = Number(value);
  if (!Number.isFinite(amount)) return "";
  if (resource === "cpu") return `${amount.toFixed(1)} cores`;
  const units = ["bytes", "KiB", "MiB", "GiB", "TiB"];
  let size = amount;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  const text = unit === 0 ? String(Math.round(size)) : size.toFixed(size >= 10 ? 0 : 1);
  return `${text} ${units[unit]}`;
}

function forecastTaskRef(forecast, taskId) {
  const title = forecast.task_titles?.[taskId];
  const link = taskLink(String(taskId), selectedWorkspace()?.id);
  if (!title) return link;
  return el("span", { class: "drain-forecast-task-name" }, [link, " ", String(title)]);
}

function forecastHasDecisions(forecast) {
  if (!forecast) return false;
  const admitted = Array.isArray(forecast.admitted) && forecast.admitted.some(task => task?.task_id);
  const skipped = Array.isArray(forecast.skipped) && forecast.skipped.some(task => task?.task_id);
  return admitted || skipped || Boolean(forecast.head?.task_id && (forecast.head.protected === true || forecast.head.stall?.stuck === true));
}

function forecastTime(at) {
  const node = el("time", { class: "drain-forecast-when", text: time(at), title: String(at) });
  node.dateTime = String(at);
  return node;
}

// The minutes a forecast's recorded series spans.
const FORECAST_SERIES_MINUTES = 60;

// `resource`'s recorded first hour of percents, or null for a record written
// before the series was kept or from an unmeasured host.
function forecastSeries(forecast, resource) {
  const series = asForecast(forecast.series);
  const bucket = Number(series?.bucket_minutes);
  const points = series?.[`${resource}_percent`];
  if (!(bucket > 0) || !Array.isArray(points) || points.length < 2) return null;
  const values = points.map(value => (value == null ? NaN : Number(value)));
  return values.every(Number.isFinite) ? { bucket, values } : null;
}

// The series as a line over 0–60 minutes on a 0–100% scale (taller when a
// point or the limit is above it), the admit threshold as a dashed line, and
// a dot on the highest point labelled with the peak's minute. That point is
// the peak's bucket whenever the peak falls within the hour; otherwise the
// label names the bucket, and the row's text the peak.
function forecastSpark(series, peak, admitNumber, label, hypothetical) {
  const { bucket, values } = series;
  const limited = Number.isFinite(admitNumber);
  const top = Math.max(100, ...values, limited ? admitNumber : 0);
  const x = index => Math.min(100, index * bucket / FORECAST_SERIES_MINUTES * 100);
  const y = value => 100 - Math.max(0, value) / top * 100;
  const highest = values.indexOf(Math.max(...values));
  const peakMinute = Number(peak?.minute);
  const inHour = Number.isFinite(peakMinute) && peakMinute <= FORECAST_SERIES_MINUTES && Math.ceil(peakMinute / bucket) === highest;
  const minute = inHour ? peakMinute : highest * bucket;
  const extra = limited
    ? [svg("line", { x1: 0, x2: 1000, y1: (y(admitNumber) * 10).toFixed(1), y2: (y(admitNumber) * 10).toFixed(1), class: "drain-forecast-spark-limit", "vector-effect": "non-scaling-stroke" })]
    : [];
  const line = sparkline(values.map((value, index) => ({ x: x(index), y: y(value) })), { className: "drain-forecast-spark-svg", lineClass: "drain-forecast-spark-line", extra });
  const left = x(highest);
  const edge = left > 75 ? " end" : left < 25 ? " start" : "";
  const marker = el("span", { class: "drain-forecast-spark-peak", style: { left: `${left}%`, top: `${y(values[highest])}%` } });
  const chart = el("span", { class: `drain-forecast-spark${hypothetical ? " hypothetical" : ""}`, role: "img", "aria-label": label }, [
    el("span", { class: `drain-forecast-spark-label${edge}`, text: `+${minute}m`, style: { left: `${left}%` }, "aria-hidden": "true" }),
    el("span", { class: "drain-forecast-spark-plot", "aria-hidden": "true" }, [line, marker]),
  ]);
  chart.dataset.peakMinute = String(minute);
  return chart;
}

// Each peak against its admit threshold, with both figures beside it: a
// sparkline of the forecast's first hour when the record keeps one, or else
// (and always in the Tasks dock) a meter whose fill is the peak and whose
// tick is the threshold. The amount,
// minute and headroom (in percentage points) are in the row's tooltip and
// read aloud. A shadow forecast's chart is marked hypothetical.
function forecastPeaks(forecast) {
  const headroom = asForecast(forecast.headroom);
  const hypothetical = forecast.mode === "shadow";
  const rows = ["cpu", "memory"].map((resource) => {
    const peak = asForecast(forecast[`${resource}_peak`]);
    if (!peak || (!Number.isFinite(Number(peak.value)) && !Number.isFinite(Number(peak.percent)))) return null;
    const percent = Number(peak.percent);
    const known = Number.isFinite(percent);
    const figure = known ? `${percent.toFixed(1)}%` : "host capacity unknown";
    const amount = forecastAmount(resource, peak.value);
    const when = Number.isFinite(Number(peak.minute)) ? `at +${peak.minute}m` : "";
    const admitKey = resource === "cpu" ? "admit_cpu_percent" : "admit_memory_percent";
    const roomKey = resource === "cpu" ? "cpu_percent" : "memory_percent";
    // An unmeasured host sends its headroom as null, and Number(null) is 0.
    const admitNumber = headroom?.[admitKey] == null ? NaN : Number(headroom[admitKey]);
    const admit = headroom
      ? (Number.isFinite(admitNumber) ? `admit ${headroom[admitKey]}%` : "admit threshold unknown")
      : "";
    const roomNumber = headroom?.[roomKey] == null ? NaN : Number(headroom[roomKey]);
    const room = headroom ? (Number.isFinite(roomNumber) ? `headroom ${roomNumber.toFixed(1)}` : "headroom unknown") : "";
    const over = Number.isFinite(roomNumber) && roomNumber < 0;
    const name = resource === "cpu" ? "CPU" : "Memory";
    const detail = `${name} peak ${[`${figure}${amount ? ` (${amount})` : ""}`, when, admit, room].filter(Boolean).join(" · ")}`;
    const series = known ? forecastSeries(forecast, resource) : null;
    const clamp = value => `${Math.min(100, Math.max(0, value))}%`;
    const meter = el("span", { class: "drain-forecast-meter", "aria-hidden": "true" }, [
      known ? el("span", { class: "drain-forecast-fill", style: { width: clamp(percent) } }) : null,
      Number.isFinite(admitNumber) ? el("span", { class: "drain-forecast-tick", style: { left: clamp(admitNumber) } }) : null,
    ]);
    // With a series the chart leads and the meter follows it for the narrow
    // Tasks dock, which draws the meter instead (drain.css, tasks.css).
    const graphic = [meter];
    if (series) {
      const limit = Number.isFinite(admitNumber) ? `admit limit ${admitNumber}%` : "admit limit unknown";
      const label = `${hypothetical ? "Hypothetical " : ""}${name} forecast over the next ${FORECAST_SERIES_MINUTES} minutes: peak ${figure} ${when}, ${limit}`;
      graphic.unshift(forecastSpark(series, peak, admitNumber, label, hypothetical));
    }
    // The figure reads as the peak against its admit threshold, then when it
    // peaks, the amount and how far it sits from that ceiling, in percentage
    // points.
    const margin = Number.isFinite(roomNumber)
      ? `${Math.abs(roomNumber).toFixed(1)} pts ${roomNumber < 0 ? "over" : "under"} the ceiling`
      : "";
    return el("div", { class: `drain-forecast-peak${over ? " over" : ""}`, title: detail }, [
      el("dt", { text: name }),
      el("dd", { class: "drain-forecast-bar" }, graphic),
      el("dd", { class: "drain-forecast-figure" }, [
        el("span", { class: "drain-forecast-figure-main mono", "aria-hidden": "true" }, [
          known ? `${percent.toFixed(1)}%` : "host capacity unknown",
          Number.isFinite(admitNumber) ? el("span", { class: "drain-forecast-threshold", text: ` / ${admitNumber}%` }) : null,
        ]),
        el("span", { class: "drain-forecast-figure-sub", "aria-hidden": "true", text: [when ? `peak ${when}` : "", amount, margin].filter(Boolean).join(" · ") }),
        el("span", { class: "visually-hidden", text: detail }),
      ]),
    ]);
  }).filter(Boolean);
  if (rows.length === 0) return null;
  const list = el("dl", { class: "drain-forecast-peaks" }, rows);
  list.setAttribute("aria-label", "Forecast peaks against admit thresholds");
  return list;
}

// Live decisions use present tense; a shadow decision says what this pass
// would do rather than what it will do.
function forecastVerbs(hypothetical) {
  return {
    admit: hypothetical ? "Would admit " : "Admitted ",
    hold: hypothetical ? "Would hold " : "Held ",
    guard: hypothetical ? "Starvation guard would protect " : "Starvation guard protects ",
    stuck: hypothetical ? "Stuck head (shadow; hypothetical): " : "Stuck head: ",
    improved: "has not improved",
  };
}

function forecastDecisions(forecast, hypothetical) {
  const items = [];
  const verbs = forecastVerbs(hypothetical);
  for (const task of Array.isArray(forecast.admitted) ? forecast.admitted : []) {
    if (!task?.task_id) continue;
    const notes = [`profile ${task.profile_source || "unknown"}`];
    if (task.idle_host) notes.push(String(task.idle_host));
    // An admitted task stays on one line; its full text is the tooltip.
    const item = el("li", { class: "drain-forecast-task admitted" }, [
      verbs.admit,
      forecastTaskRef(forecast, task.task_id),
      ` · ${notes.join(" · ")}`,
    ]);
    item.title = item.textContent;
    items.push(item);
  }
  for (const task of Array.isArray(forecast.skipped) ? forecast.skipped : []) {
    if (!task?.task_id) continue;
    items.push(el("li", { class: "drain-forecast-task held" }, [
      verbs.hold,
      forecastTaskRef(forecast, task.task_id),
      ` · ${forecastHoldDetail(task)} · profile ${task.profile_source || "unknown"}`,
    ]));
  }
  if (forecast.head?.protected === true && forecast.head.task_id) {
    items.push(el("li", { class: "drain-forecast-task held" }, [
      verbs.guard,
      forecastTaskRef(forecast, forecast.head.task_id),
    ]));
  }
  const stall = forecast.head?.stall;
  if (stall?.stuck === true && forecast.head.task_id) {
    const miss = forecast.head.miss || {};
    const resource = miss.resource || "resource";
    const profile = miss.profile_key
      ? `profile ${miss.profile_key} via ${miss.profile_source || "?"}`
      : `profile ${miss.profile_source || "none"}`;
    const baseline = Number.isFinite(Number(miss.baseline_percent))
      ? `${Number(miss.baseline_percent).toFixed(0)}%`
      : "unmeasured";
    const since = stall.since ? time(stall.since) : "unknown";
    items.push(el("li", { class: "drain-forecast-task held" }, [
      verbs.stuck,
      forecastTaskRef(forecast, forecast.head.task_id),
      ` — ${forecastHoldDetail(miss)} (${profile}; ${resource} baseline ${baseline}) ${verbs.improved} over ${stall.passes ?? "?"} protected passes since ${since}.`,
    ]));
  }
  if (items.length === 0) return null;
  const list = el("ul", { class: "drain-forecast-tasks" }, items);
  const label = hypothetical ? "Hypothetical forecast decisions" : "Forecast decisions";
  list.setAttribute("aria-label", label);
  return list;
}

function forecastHead(label, ...meta) {
  return el("div", { class: "drain-forecast-head" }, [
    el("span", { class: "drain-section-label", text: label }),
    el("span", { class: "drain-forecast-meta" }, meta.filter(Boolean)),
  ]);
}

function forecastBlock(forecast, { retained, windowNote = null }) {
  const hypothetical = forecast.mode === "shadow";
  const at = forecastBuiltAt(forecast);
  const block = el("div", { class: retained ? "drain-forecast-retained" : "drain-forecast-built" });
  block.dataset.forecastMode = forecast.mode || "";
  block.append(
    forecastHead(
      retained ? "Last forecast" : "Forecast",
      at ? forecastTime(at) : null,
      at ? " · " : null,
      el("span", { class: `drain-forecast-mode${hypothetical ? " shadow" : ""}`, text: hypothetical ? "shadow · hypothetical" : (forecast.mode || "forecast") }),
    ),
    ...[
      windowNote ? el("p", { class: "drain-forecast-ended", text: windowNote }) : null,
      forecastPeaks(forecast),
      retained ? null : forecastDecisions(forecast, hypothetical),
    ].filter(Boolean),
  );
  return block;
}

function clearBacklogForecastCache() {
  backlogForecastCache = { revision: null, status: "pending", summary: null };
  backlogForecastFlight = null;
}

// Paint the settled summary into the admission section. Pending, and a
// revision that is not the one on screen, leave the slot empty so the card
// does not shift while the read is in flight.
function placeBacklogForecast(section) {
  if (!section) return;
  section.querySelector(":scope > .drain-backlog-forecast")?.remove();
  if (backlogForecastCache.revision !== getWorkspaceRevision() || backlogForecastCache.status === "pending") return;
  const summary = backlogForecastCache.status === "ready" ? backlogForecastCache.summary : null;
  section.append(drainBacklogForecastLink(summary));
}

function paintBacklogForecast() {
  placeBacklogForecast($("auto-drain-body")?.querySelector(".drain-forecast"));
}

// The default backlog forecast, independent of the drain refresh. A failure or a
// payload with no time-to-drain estimate becomes the muted line and never
// rejects the drain card. Aggregate and unselected views do not ask.
export function refreshBacklogForecast() {
  const tab = operationsState.context?.getTab?.();
  // The forecast is shown in the Tasks dock. Other
  // dashboard polls refresh readiness without needing this optional read.
  if (!selectedWorkspace() || isAggregateView() || tab !== "tasks") return Promise.resolve();
  const revision = getWorkspaceRevision();
  if (backlogForecastFlight && backlogForecastFlight.revision === revision) return backlogForecastFlight.promise;
  const promise = fetchBacklogForecast().then((payload) => {
    if (getWorkspaceRevision() !== revision || !selectedWorkspace()) return;
    const summary = backlogForecastSummary(payload);
    backlogForecastCache = { revision, status: summary ? "ready" : "unavailable", summary };
  }).catch(() => {
    if (getWorkspaceRevision() !== revision || !selectedWorkspace()) return;
    backlogForecastCache = { revision, status: "unavailable", summary: null };
  }).finally(() => {
    if (backlogForecastFlight?.promise === promise) backlogForecastFlight = null;
    if (getWorkspaceRevision() === revision) paintBacklogForecast();
  });
  backlogForecastFlight = { revision, promise };
  return promise;
}

// A retained forecast whose drain has ended, with no pull drain and no
// winding-down children, is not a live window. Shared occupancy is read apart
// from the drain's own phase, so the note claims nothing is running only when
// that count is exactly zero. A positive count names the other work, and an
// unknown count says only that the window ended.
function retainedWindowNote(payload, forecast) {
  if (forecast?.drain_live !== false) return null;
  const capacity = payload?.capacity || {};
  if (capacity.pull_drain_run_id) return null;
  const phase = capacity.drain_phase;
  if (phase != null && phase !== "idle") return null;
  const busy = autoDrainNumber(capacity.active_leaf_runs ?? capacity.occupancy?.active_leaf_runs);
  if (busy === 0) return "window ended · nothing running";
  if (busy > 0) return "window ended · other work running";
  return "window ended";
}

// The last local pass's gap-fit forecast. Readiness puts the pass on
// `capacity.admission_forecast` and the last forecast that was actually built
// on `capacity.last_admission_forecast`. A pass that built none says so, unless
// a retained forecast stands in and this pass decided nothing of its own.
// The backlog summary, when settled, is the last child of this section.
export function autoDrainForecast(payload) {
  const capacity = payload?.capacity || {};
  const current = asForecast(capacity.admission_forecast);
  const last = asForecast(capacity.last_admission_forecast);
  const section = el("div", { class: "drain-forecast", role: "group", "aria-label": "Admission forecast" });
  const currentBuilt = forecastBuiltAt(current);
  const currentRetained = currentBuilt && current.drain_live === false;
  if (currentBuilt && !currentRetained) {
    section.dataset.forecastState = "built";
    section.append(forecastBlock(current, { retained: false }));
    placeBacklogForecast(section);
    return section;
  }
  const retainedForecast = currentRetained ? current : last;
  const retained = forecastBuiltAt(retainedForecast);
  section.dataset.forecastState = retained ? "retained" : "none";
  const decisions = !currentRetained && forecastHasDecisions(current)
    ? forecastDecisions(current, current.mode === "shadow")
    : null;
  if (decisions || !retained) {
    section.append(forecastHead("Forecast", el("span", { class: "drain-forecast-state", text: "no forecast this pass" })));
    if (decisions) section.append(decisions);
  }
  if (retained) {
    section.append(forecastBlock(retainedForecast, {
      retained: true,
      windowNote: retainedWindowNote(payload, retainedForecast),
    }));
  }
  placeBacklogForecast(section);
  return section;
}

// The status card reads top to bottom: the live window's figures (if any),
// the slots it competes for and why nothing more starts, any provider or crew
// hold, the last admission forecast, the backlog forecast summary under it,
// then the run and Stop. Its deadline sits
// in the header. Start a window is its own card, hidden in a read-only view.

registerOperationsReset(clearBacklogForecastCache);

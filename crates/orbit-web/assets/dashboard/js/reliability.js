// Orbit dashboard pipeline-reliability rendering [ORB-10588].
// Pure vanilla JS, split into ES modules with no build step.
//
// Renders `GET /api/metrics/reliability`: job-run failure rate and recovery
// invocation rate, both computed server-side from durable `job_runs` /
// `invocations` rows. Nothing here derives from tokens or cost.
//
// Two display rules are load-bearing and apply to every rate on this view:
//
//   1. A percentage is never shown without its denominator and its window.
//      `fmtRate` renders "12.5% (n=48)" and the panel header states the
//      window, so a number can't be read out of context.
//   2. A rate whose denominator is below the server's confidence threshold
//      (`low_sample`, set in orbit-core) is withheld — the cell shows the raw
//      counts and an "n too small" marker instead of a percentage.
//
// The server also reports which run states it counted. `succeeded + failed`
// is smaller than the run total whenever runs were cancelled, skipped, or are
// still in flight, so the summary spells out the excluded bucket rather than
// letting the two visible numbers imply they add up.

import { el, syncNodes, fetchJson, getWindow, payloadHonorsWindow, reliabilityWindowFor, wireWindowSelector, syncWindowSelectors } from './common.js';

const $ = (id) => document.getElementById(id);

// Mirrors the windows the endpoint accepts. `all` is deliberately absent:
// a failure rate with no time range is not actionable. When the dashboard
// window is `all`, Reliability keeps a labeled independent 7d cutoff.
const RELIABILITY_WINDOWS = ["1h", "24h", "7d", "30d"];
const DEFAULT_RELIABILITY_WINDOW = "7d";

function getReliabilityWindow() {
  return reliabilityWindowFor(getWindow()).window;
}

const pctFormatter = new Intl.NumberFormat("en-US", {
  style: "percent",
  maximumFractionDigits: 1,
});

function num(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

/// Renders a `Rate` from the API. Returns display text plus the CSS modifier
/// the caller applies, so withheld and low-confidence cells are visually
/// distinct from a real reading.
function describeRate(rate) {
  if (!rate || num(rate.denominator) === 0) {
    return {
      text: "n/a",
      cls: "rel-rate rel-rate-empty",
      title: rate
        ? `no ${rate.denominator_label} in this window`
        : "no data",
    };
  }
  const n = num(rate.denominator);
  const detail = `${num(rate.numerator)} / ${n} ${rate.denominator_label}`;
  if (rate.low_sample) {
    // Withheld, not rounded: a 1-in-3 sample rendered as "33%" reads far more
    // confident than the evidence supports.
    return {
      text: `${num(rate.numerator)}/${n} · n too small`,
      cls: "rel-rate rel-rate-low",
      title: `${detail} — below the confidence threshold, percentage withheld`,
    };
  }
  return {
    text: `${pctFormatter.format(num(rate.value))} (n=${n})`,
    cls: "rel-rate",
    title: detail,
  };
}

function rateCell(rate, extraClass = "") {
  const described = describeRate(rate);
  const node = el("span", {
    class: `${described.cls} ${extraClass}`.trim(),
    text: described.text,
    title: described.title,
  });
  return node;
}

function fmtWindowRange(window) {
  if (!window) return "";
  const since = new Date(window.since);
  const until = new Date(window.until);
  if (Number.isNaN(since.getTime()) || Number.isNaN(until.getTime())) return "";
  const opts = { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" };
  return `${since.toLocaleString(undefined, opts)} → ${until.toLocaleString(undefined, opts)} UTC${
    window.bucket ? ` · ${window.bucket} buckets` : ""
  }`;
}

function statTile(label, value, hint) {
  return el("div", { class: "rel-tile" }, [
    el("span", { class: "rel-tile-label", text: label }),
    typeof value === "string" ? el("span", { class: "rel-tile-value", text: value }) : value,
    hint ? el("span", { class: "rel-tile-hint", text: hint }) : null,
  ]);
}

/// The headline block: both rates, each with its `n`, plus the explicit
/// statement of what the failure-rate denominator excludes.
function renderSummary(payload) {
  const host = $("reliability-summary");
  if (!host) return;
  const totals = payload.totals || {};
  const runs = totals.job_runs || {};
  const counts = runs.counts || {};
  const recovery = totals.recovery || {};

  const settled = num(counts.succeeded) + num(counts.failed);
  const excluded =
    num(counts.cancelled) + num(counts.skipped) + num(counts.in_flight) + num(counts.unknown);

  const tiles = [
    statTile(
      "Job-run failure rate",
      rateCell(runs.failure_rate),
      `${num(counts.failed)} failed of ${settled} settled runs`,
    ),
    statTile(
      "Recovery per step invocation",
      rateCell(recovery.per_step_invocation),
      recovery.per_step_invocation
        ? `denominator: ${recovery.per_step_invocation.denominator_label}`
        : "",
    ),
    statTile(
      "Job runs needing recovery",
      rateCell(recovery.per_job_run),
      recovery.per_job_run
        ? `denominator: ${recovery.per_job_run.denominator_label}`
        : "",
    ),
    statTile(
      "Runs in window",
      String(num(counts.total)),
      `${settled} settled · ${excluded} not counted`,
    ),
  ];

  const nodes = tiles.map((tile, index) => {
    tile.dataset.key = `rel-tile-${index}`;
    tile.dataset.hash = tile.textContent;
    return tile;
  });
  syncNodes(host, nodes);

  const note = $("reliability-denominator-note");
  if (note) {
    // States that are terminal-but-not-an-outcome, or not terminal at all, sit
    // outside the rate. Saying so here is what keeps "success + failed" from
    // reading as the whole population.
    note.textContent =
      excluded > 0
        ? `Failure rate is over settled runs only (success + failed = ${settled}). ` +
          `${excluded} further run(s) in this window were cancelled, skipped, still in flight, ` +
          `or in an unrecognized state and are excluded from the denominator.`
        : `Failure rate is over settled runs only (success + failed = ${settled}). ` +
          `No cancelled, skipped, in-flight, or unrecognized runs in this window.`;
  }

  const truncNote = $("reliability-truncation-note");
  if (truncNote) {
    truncNote.style.display = runs.truncated ? "" : "none";
    truncNote.textContent = runs.truncated
      ? "Window exceeded the per-read row cap; older runs in this window are not included in these counts."
      : "";
  }
}

function tableFrom(columns, rows, keyFn) {
  const table = el("table", { class: "scoreboard-table rel-table" });
  const thead = el("thead");
  const headRow = el("tr");
  for (const col of columns) {
    headRow.appendChild(el("th", { class: col.num ? "num" : "", text: col.label }));
  }
  thead.appendChild(headRow);
  table.appendChild(thead);

  const tbody = el("tbody");
  for (const row of rows) {
    const tr = el("tr");
    for (const col of columns) {
      const td = el("td", { class: col.num ? "num" : "" });
      const rendered = col.render(row);
      if (typeof rendered === "string") {
        td.textContent = rendered;
      } else if (rendered) {
        td.appendChild(rendered);
      }
      tr.appendChild(td);
    }
    tr.dataset.key = keyFn(row);
    tr.dataset.hash = tr.textContent;
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  return table;
}

function emptyState(text) {
  const node = el("div", { class: "empty-state" }, [
    el("div", { class: "icon", text: "✧" }),
    el("div", { class: "text", text }),
  ]);
  node.dataset.key = "empty";
  node.dataset.hash = text;
  return node;
}

/// Failure rate broken out by workspace and by job — the two axes that make a
/// spike attributable rather than merely alarming.
function renderBreakdown(payload) {
  const host = $("reliability-breakdown");
  if (!host) return;
  const workspaces = payload.workspaces || [];
  if (workspaces.length === 0) {
    syncNodes(host, [emptyState("No job runs in this window.")]);
    return;
  }

  const cards = [];
  const wsRows = workspaces.map((ws) => ({
    key: ws.workspace_id,
    label: ws.workspace_name || ws.workspace_id,
    counts: (ws.job_runs || {}).counts || {},
    rate: (ws.job_runs || {}).failure_rate,
  }));
  cards.push(
    card("Failure rate by workspace", tableFrom(
      [
        { label: "workspace", render: (r) => r.label },
        { label: "runs", num: true, render: (r) => String(num(r.counts.total)) },
        { label: "settled", num: true, render: (r) => String(num(r.counts.succeeded) + num(r.counts.failed)) },
        { label: "failed", num: true, render: (r) => String(num(r.counts.failed)) },
        { label: "failure rate", num: true, render: (r) => rateCell(r.rate) },
      ],
      wsRows,
      (r) => `ws-${r.key}`,
    ), "rel-card-workspaces"),
  );

  // Jobs are flattened across workspaces and tagged, so the worst job leads
  // regardless of which workspace it belongs to.
  const jobRows = [];
  for (const ws of workspaces) {
    for (const job of (ws.job_runs || {}).by_job || []) {
      jobRows.push({
        key: `${ws.workspace_id}::${job.job_id}`,
        job_id: job.job_id,
        workspace: ws.workspace_name || ws.workspace_id,
        counts: job.counts || {},
        rate: job.failure_rate,
      });
    }
  }
  jobRows.sort((a, b) => num(b.counts.failed) - num(a.counts.failed) || num(b.counts.total) - num(a.counts.total));
  cards.push(
    card("Failure rate by job", jobRows.length
      ? tableFrom(
          [
            { label: "job", render: (r) => r.job_id },
            { label: "workspace", render: (r) => r.workspace },
            { label: "runs", num: true, render: (r) => String(num(r.counts.total)) },
            { label: "failed", num: true, render: (r) => String(num(r.counts.failed)) },
            { label: "failure rate", num: true, render: (r) => rateCell(r.rate) },
          ],
          jobRows,
          (r) => `job-${r.key}`,
        )
      : emptyState("No job runs in this window."),
      "rel-card-jobs"),
  );

  syncNodes(host, cards);
}

function card(title, body, key) {
  const node = el("div", { class: "audit-summary-card rel-card" }, [
    el("div", { class: "card-title", text: title }),
    el("div", { class: "card-body" }, [body]),
  ]);
  node.dataset.key = key;
  node.dataset.hash = node.textContent;
  return node;
}

/// The over-time series. Each bar carries its own `n` in the tooltip, and a
/// bucket whose denominator is too small is drawn as a muted "low n" bar
/// rather than as a tall confident spike.
function renderOverTime(payload) {
  const host = $("reliability-over-time");
  if (!host) return;
  const workspaces = payload.workspaces || [];

  // Sum buckets across workspaces by start time so the series is machine-wide.
  const merged = new Map();
  for (const ws of workspaces) {
    for (const bucket of (ws.job_runs || {}).over_time || []) {
      const existing = merged.get(bucket.bucket_start) || {
        bucket_start: bucket.bucket_start,
        succeeded: 0,
        failed: 0,
        total: 0,
      };
      existing.succeeded += num((bucket.counts || {}).succeeded);
      existing.failed += num((bucket.counts || {}).failed);
      existing.total += num((bucket.counts || {}).total);
      merged.set(bucket.bucket_start, existing);
    }
  }
  const series = Array.from(merged.values()).sort((a, b) =>
    a.bucket_start < b.bucket_start ? -1 : a.bucket_start > b.bucket_start ? 1 : 0,
  );

  if (series.length === 0) {
    syncNodes(host, [emptyState("No job runs in this window.")]);
    return;
  }

  const bars = series.map((point, index) => {
    const settled = point.succeeded + point.failed;
    const rate = settled > 0 ? point.failed / settled : null;
    const lowSample = settled > 0 && settled < 20;
    const bar = el("div", { class: "rel-bar-slot" });
    const fill = el("div", {
      class: `rel-bar${rate == null ? " rel-bar-empty" : lowSample ? " rel-bar-low" : ""}`,
    });
    // A bucket with no settled runs gets a hairline, not a zero-height bar
    // that would read identically to a clean 0% bucket.
    fill.style.height = rate == null ? "2px" : `${Math.max(2, rate * 100)}%`;
    bar.appendChild(fill);
    bar.title =
      rate == null
        ? `${new Date(point.bucket_start).toLocaleString()} — no settled runs (n=0)`
        : `${new Date(point.bucket_start).toLocaleString()} — ${pctFormatter.format(rate)} failed` +
          ` (${point.failed}/${settled} settled${lowSample ? ", n too small to be confident" : ""})`;
    bar.dataset.key = `bucket-${point.bucket_start}`;
    bar.dataset.hash = `${point.failed}-${settled}`;
    return bar;
  });

  syncNodes(host, bars);
  const axis = $("reliability-over-time-axis");
  if (axis && series.length > 0) {
    axis.textContent = `${new Date(series[0].bucket_start).toLocaleString()} → ${new Date(
      series[series.length - 1].bucket_start,
    ).toLocaleString()}`;
  }
}

/// Per-activity invocation counts with the role each activity was discovered
/// to play. This is the audit trail for the recovery rate: the numerator's
/// membership is visible, not asserted.
function renderActivities(payload) {
  const host = $("reliability-activities");
  if (!host) return;
  const rows = [];
  for (const ws of payload.workspaces || []) {
    for (const activity of (ws.recovery || {}).by_activity || []) {
      rows.push({
        key: `${ws.workspace_id}::${activity.activity_id}`,
        activity_id: activity.activity_id,
        workspace: ws.workspace_name || ws.workspace_id,
        invocation_count: num(activity.invocation_count),
        job_run_count: num(activity.job_run_count),
        role: activity.role || "unknown",
      });
    }
  }
  rows.sort((a, b) => b.invocation_count - a.invocation_count);

  if (rows.length === 0) {
    syncNodes(host, [emptyState("No invocations recorded in this window.")]);
    return;
  }

  syncNodes(host, [
    card("Invocations by activity and discovered role", tableFrom(
      [
        { label: "activity", render: (r) => r.activity_id },
        { label: "role", render: (r) => el("span", { class: `rel-role rel-role-${r.role}`, text: r.role }) },
        { label: "workspace", render: (r) => r.workspace },
        { label: "invocations", num: true, render: (r) => String(r.invocation_count) },
        { label: "runs touched", num: true, render: (r) => String(r.job_run_count) },
      ],
      rows,
      (r) => `act-${r.key}`,
    ), "rel-card-activities"),
  ]);
}

function renderReliabilityScope(payload) {
  const badge = $("reliability-scope-badge");
  if (!badge) return;
  const scope = payload && payload.scope === "workspace" ? "Workspace" : "Fleet-wide";
  const rel = reliabilityWindowFor(getWindow());
  badge.textContent = rel.independent ? `${scope} · independent ${rel.window}` : scope;
  badge.className = "scope-badge independent";
  badge.hidden = false;
  badge.title = rel.independent
    ? "Reliability is fleet-wide and cannot use the unbounded all window"
    : "Reliability is fleet-wide; the workspace selector does not apply";
}

function renderReliability(payload) {
  if (!payload) return;
  const rel = reliabilityWindowFor(getWindow());
  if (!payloadHonorsWindow(payload, rel.window)) {
    console.error(
      `reliability payload window ${(payload.window || {}).label} rejected under ${rel.window} selection`,
    );
    return;
  }
  syncWindowSelectors();
  renderReliabilityScope(payload);
  const meta = $("reliability-meta");
  if (meta) {
    const range = fmtWindowRange(payload.window);
    const unreadable = (payload.unreadable_workspaces || []).length;
    const fleet = "Fleet-wide";
    const independent = rel.independent ? " · independent window" : "";
    meta.textContent = `${fleet} · ${range}${independent}${unreadable ? ` · ${unreadable} workspace(s) unreadable` : ""}`;
  }
  const count = $("reliability-count");
  if (count) {
    const total = num(((payload.totals || {}).job_runs || {}).counts?.total);
    count.textContent = `${total} runs / ${(payload.window || {}).label || ""}`;
  }
  renderSummary(payload);
  renderOverTime(payload);
  renderBreakdown(payload);
  renderActivities(payload);
}

function fetchAndRenderReliability() {
  const rel = reliabilityWindowFor(getWindow());
  return fetchJson(`/api/metrics/reliability?window=${encodeURIComponent(rel.window)}`)
    .then(renderReliability);
}

// Idempotent attach, matching the scoreboard selector's contract.
function wireReliabilityWindowSelector() {
  wireWindowSelector("reliability-window-selector", { allowAll: false });
}

export {
  renderReliability,
  fetchAndRenderReliability,
  wireReliabilityWindowSelector,
  getReliabilityWindow,
  describeRate,
  RELIABILITY_WINDOWS,
  DEFAULT_RELIABILITY_WINDOW,
};

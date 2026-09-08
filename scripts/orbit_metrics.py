#!/usr/bin/env python3
"""
Orbit Metrics Trend Visualizer
Reads all metrics JSONL files from .orbit/state/diagnostics/metrics/
and renders interactive trend charts.
"""

import json
import sys
from pathlib import Path
from collections import defaultdict
from datetime import datetime, timezone

try:
    import plotly.graph_objects as go
    from plotly.subplots import make_subplots
except ImportError:
    print("Installing plotly...")
    import subprocess
    subprocess.check_call([sys.executable, "-m", "pip", "install", "plotly", "-q"])
    import plotly.graph_objects as go
    from plotly.subplots import make_subplots


# ── Config ────────────────────────────────────────────────────────────────────

REPO_ROOT   = Path(__file__).resolve().parent.parent
METRICS_DIR = REPO_ROOT / ".orbit/state/diagnostics/metrics"
OUTPUT_HTML = REPO_ROOT / ".orbit/state/metrics_trends.html"

# Steps worth tracking individually for duration trends
DURATION_STEPS = {
    "implement_change",
    "dispatch_and_plan_batch",
    "plan_batch_tasks",
    "review_batch_pr",
    "review_pr",
    "finalize_tasks",
}

STEP_COLORS = {
    "implement_change":       "#4C9BE8",
    "dispatch_and_plan_batch":"#F5A623",
    "plan_batch_tasks":       "#F5A623",
    "review_batch_pr":        "#7ED321",
    "review_pr":              "#50E3C2",
    "finalize_tasks":         "#D0021B",
}


# ── Parsing ───────────────────────────────────────────────────────────────────

def parse_actor(raw) -> str:
    """Render an ActorIdentity: a flat label string, or the tagged
    ``{"human": ...}`` / ``{"agent": {...}}`` shape used for the labels a flat
    string cannot represent."""
    if isinstance(raw, str):
        return raw
    if isinstance(raw, dict):
        if isinstance(raw.get("human"), str):
            return raw["human"] or "unknown"
        agent = raw.get("agent", {})
        name = agent.get("name", "")
        model = agent.get("model", "")
        if name and model:
            return f"{name} / {model}"
        return model or name or "unknown"
    return "unknown"


def load_all_metrics(base: Path) -> list[dict]:
    records = []
    for jsonl in sorted(base.rglob("*.jsonl")):
        for line in jsonl.read_text().splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                r = json.loads(line)
                r["_ts"] = datetime.fromisoformat(r["ts"].replace("Z", "+00:00"))
                r["_date"] = r["_ts"].date()
                r["_actor"] = parse_actor(r.get("actor_identity", "unknown"))
                records.append(r)
            except Exception:
                pass
    records.sort(key=lambda r: r["_ts"])
    return records


# ── Aggregation ───────────────────────────────────────────────────────────────

def by_date(records, key_fn, val_fn, filter_fn=None):
    """Group val_fn(r) by (date, key_fn(r)), skipping None values."""
    out = defaultdict(lambda: defaultdict(list))
    for r in records:
        if filter_fn and not filter_fn(r):
            continue
        v = val_fn(r)
        if v is None:
            continue
        out[r["_date"]][key_fn(r)].append(v)
    return out


# ── Chart builders ────────────────────────────────────────────────────────────

def chart_steps_per_day(records, fig, row, col):
    date_counts = defaultdict(int)
    for r in records:
        date_counts[r["_date"]] += 1

    dates = sorted(date_counts)
    counts = [date_counts[d] for d in dates]

    fig.add_trace(go.Bar(
        x=dates, y=counts,
        name="Steps / day",
        marker_color="#4C9BE8",
        showlegend=False,
    ), row=row, col=col)
    fig.update_yaxes(title_text="Count", row=row, col=col)


def chart_duration_by_step(records, fig, row, col):
    grouped = by_date(
        records,
        key_fn=lambda r: r["step"],
        val_fn=lambda r: r.get("step_duration_ms"),
        filter_fn=lambda r: r["step"] in DURATION_STEPS,
    )

    all_dates = sorted({r["_date"] for r in records})
    added = set()

    for step in DURATION_STEPS:
        dates, medians = [], []
        for d in all_dates:
            vals = grouped[d].get(step)
            if vals:
                dates.append(d)
                medians.append(sum(vals) / len(vals) / 1000)  # → seconds
        if not dates:
            continue
        color = STEP_COLORS.get(step, "#888")
        fig.add_trace(go.Scatter(
            x=dates, y=medians,
            mode="lines+markers",
            name=step,
            line=dict(color=color, width=2),
            marker=dict(size=6),
            showlegend=step not in added,
        ), row=row, col=col)
        added.add(step)

    fig.update_yaxes(title_text="Avg duration (s)", row=row, col=col)


def chart_token_usage(records, fig, row, col):
    grouped = by_date(
        records,
        key_fn=lambda r: "tokens",
        val_fn=lambda r: r.get("token_usage") if isinstance(r.get("token_usage"), (int, float)) else None,
    )
    dates = sorted(grouped)
    totals = [sum(grouped[d]["tokens"]) for d in dates]

    fig.add_trace(go.Bar(
        x=dates, y=totals,
        name="Tokens / day",
        marker_color="#7ED321",
        showlegend=False,
    ), row=row, col=col)
    fig.update_yaxes(title_text="Total tokens", row=row, col=col)


def chart_tool_invocations(records, fig, row, col):
    grouped = by_date(
        records,
        key_fn=lambda r: "tools",
        val_fn=lambda r: r.get("tool_invocations") if r.get("tool_invocations") else None,
    )
    dates = sorted(grouped)
    totals = [sum(grouped[d]["tools"]) for d in dates]

    fig.add_trace(go.Bar(
        x=dates, y=totals,
        name="Tool invocations / day",
        marker_color="#BD10E0",
        showlegend=False,
    ), row=row, col=col)
    fig.update_yaxes(title_text="Tool invocations", row=row, col=col)


def chart_actor_steps(records, fig, row, col):
    actor_date = defaultdict(lambda: defaultdict(int))
    for r in records:
        actor_date[r["_actor"]][r["_date"]] += 1

    all_dates = sorted({r["_date"] for r in records})

    # Only show actors with meaningful activity
    top_actors = sorted(
        actor_date.items(),
        key=lambda kv: sum(kv[1].values()),
        reverse=True,
    )[:8]

    colors = ["#4C9BE8","#F5A623","#7ED321","#D0021B","#BD10E0","#50E3C2","#B8860B","#888"]
    for i, (actor, date_counts) in enumerate(top_actors):
        counts = [date_counts.get(d, 0) for d in all_dates]
        fig.add_trace(go.Bar(
            x=all_dates, y=counts,
            name=actor,
            marker_color=colors[i % len(colors)],
        ), row=row, col=col)

    fig.update_layout(barmode="stack")
    fig.update_yaxes(title_text="Steps", row=row, col=col)


def chart_retry_rate(records, fig, row, col):
    date_retries = defaultdict(int)
    date_total = defaultdict(int)
    for r in records:
        date_total[r["_date"]] += 1
        if r.get("retry_count", 0) > 0:
            date_retries[r["_date"]] += 1

    dates = sorted(date_total)
    rates = [100 * date_retries[d] / date_total[d] for d in dates]

    fig.add_trace(go.Scatter(
        x=dates, y=rates,
        mode="lines+markers",
        name="Retry rate %",
        line=dict(color="#D0021B", width=2),
        fill="tozeroy",
        fillcolor="rgba(208,2,27,0.1)",
        showlegend=False,
    ), row=row, col=col)
    fig.update_yaxes(title_text="Retry rate (%)", row=row, col=col)


# ── Main ──────────────────────────────────────────────────────────────────────

def main():
    if not METRICS_DIR.exists():
        sys.exit(f"Metrics directory not found: {METRICS_DIR}")

    records = load_all_metrics(METRICS_DIR)
    if not records:
        sys.exit("No metric records found.")

    date_range = f"{records[0]['_date']} → {records[-1]['_date']}"
    print(f"Loaded {len(records):,} records  ({date_range})")

    fig = make_subplots(
        rows=3, cols=2,
        subplot_titles=[
            "Steps per Day",
            "Avg Step Duration by Type (s)",
            "Token Usage per Day",
            "Tool Invocations per Day",
            "Steps by Actor (stacked)",
            "Retry Rate (%)",
        ],
        vertical_spacing=0.12,
        horizontal_spacing=0.08,
    )

    chart_steps_per_day(records,       fig, row=1, col=1)
    chart_duration_by_step(records,    fig, row=1, col=2)
    chart_token_usage(records,         fig, row=2, col=1)
    chart_tool_invocations(records,    fig, row=2, col=2)
    chart_actor_steps(records,         fig, row=3, col=1)
    chart_retry_rate(records,          fig, row=3, col=2)

    fig.update_layout(
        title=dict(
            text=f"Orbit Metrics Trends &nbsp;·&nbsp; {date_range}",
            font=dict(size=18),
        ),
        height=1000,
        template="plotly_dark",
        legend=dict(orientation="v", x=1.02, y=1),
        margin=dict(t=80, r=180),
    )

    OUTPUT_HTML.parent.mkdir(parents=True, exist_ok=True)
    fig.write_html(str(OUTPUT_HTML), include_plotlyjs="cdn")
    print(f"Saved → {OUTPUT_HTML}")

    # Also open in browser if running interactively
    if sys.stdout.isatty():
        fig.show()


if __name__ == "__main__":
    main()

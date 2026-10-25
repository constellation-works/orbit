#!/usr/bin/env python3
"""Offline decision-tree report suggesting crew performance types.

Reads the per-task implementer export produced by
``orbit agents scorecard --role implementers --per-task --json``
(from a file argument or stdin), fits a shallow hand-rolled
variance-reduction regression tree per group, and prints candidate
type definitions for an operator to accept or ignore.

It is read-only and suggests types only: it never changes draws,
pools, weights or configuration, and it writes nothing.

Target: a task's score relative to its crew's own tier mean (the
comparative-advantage target). A task's raw score is recomputed from
its per-term counts with the implementer weights, against the fleet
rates for its tier, exactly as the scorecard's tier raw is computed:
``raw = 100 + sum(weight * (actual - fleet_rate * 1) * 100)`` for one
task. The mean of those raw scores over a crew's tier tasks is that
crew's tier raw, so the relative target averages to zero per group.

Features are the task's tags and its top-level context-file path
prefixes. No machine-learning dependency is used.
"""

from __future__ import annotations

import argparse
import json
import math
import re
import sys
from collections import defaultdict

# Implementer term weights, mirroring the scorecard terms. When the
# export carries its own term weights, those are used instead.
DEFAULT_WEIGHTS = {
    "weighted_landed": 1.0,
    "first_pass_accepts": 1.0,
    "review_rejects": -1.0,
    "reviewer_repairs": -0.5,
    "crew_rework": -1.0,
    "escaped_regressions": -2.0,
}


def load_rows(source: str | None) -> tuple[list[dict], dict[str, float]]:
    text = sys.stdin.read() if source in (None, "-") else open(source, encoding="utf-8").read()
    data = json.loads(text)
    weights = dict(DEFAULT_WEIGHTS)
    if isinstance(data, dict):
        for term in data.get("terms") or []:
            if isinstance(term, dict) and "key" in term and "weight" in term:
                weights[str(term["key"])] = float(term["weight"])
        rows = data.get("tasks", data.get("rows", []))
    else:
        rows = data
    if not isinstance(rows, list):
        raise ValueError("export must be a JSON list or an object with a 'tasks' list")
    return rows, weights


def task_raw(row: dict, fleet_rates: dict[str, dict[str, float]], weights: dict[str, float]) -> float:
    counts = row.get("counts") or {}
    rates = fleet_rates.get(str(row.get("tier", "")), {})
    raw = 100.0
    for key, weight in weights.items():
        actual = float(counts.get(key, 0) or 0)
        raw += weight * (actual - rates.get(key, 0.0)) * 100.0
    return raw


def features(row: dict) -> set[str]:
    feats = {f"tag:{t}" for t in row.get("tags") or []}
    prefixes = row.get("path_prefixes")
    if prefixes is None:
        prefixes = []
        for selector in row.get("context_files") or []:
            path = str(selector)
            for pre in ("file:", "dir:"):
                if path.startswith(pre):
                    path = path[len(pre):]
                    break
            if path.startswith("symbol:"):
                path = path[len("symbol:"):].split("#", 1)[0]
            if path.startswith(("module:", "command:")):
                continue
            first = next((p for p in path.split("/") if p), "")
            if first:
                prefixes.append(first)
    feats |= {f"path:{p}" for p in prefixes}
    return feats


class Node:
    __slots__ = ("n", "mean", "feature", "present", "absent", "predicate", "parent_mean", "depth")

    def __init__(self, n, mean, predicate, parent_mean, depth):
        self.n = n
        self.mean = mean
        self.feature = None
        self.present = None
        self.absent = None
        self.predicate = predicate  # list of (feature, is_present)
        self.parent_mean = parent_mean
        self.depth = depth

    @property
    def is_leaf(self):
        return self.feature is None


def mse(values: list[float]) -> float:
    if not values:
        return 0.0
    mean = sum(values) / len(values)
    return sum((v - mean) ** 2 for v in values) / len(values)


def build(samples, max_depth, min_leaf, depth, predicate, parent_mean):
    """samples: list of (feature_set, y). Greedy variance reduction."""
    ys = [y for _, y in samples]
    node = Node(len(ys), sum(ys) / len(ys) if ys else 0.0, predicate, parent_mean, depth)
    if depth >= max_depth or len(ys) < 2 * min_leaf:
        return node
    used = {f for f, _ in predicate}
    candidates = sorted({f for feats, _ in samples for f in feats} - used)
    base = mse(ys) * len(ys)
    best = None
    for feat in candidates:
        present = [(feats, y) for feats, y in samples if feat in feats]
        absent = [(feats, y) for feats, y in samples if feat not in feats]
        if len(present) < min_leaf or len(absent) < min_leaf:
            continue
        gain = base - mse([y for _, y in present]) * len(present) - mse([y for _, y in absent]) * len(absent)
        if gain > 1e-9 and (best is None or gain > best[0]):
            best = (gain, feat, present, absent)
    if best is None:
        return node
    _, feat, present, absent = best
    node.feature = feat
    node.present = build(present, max_depth, min_leaf, depth + 1, predicate + [(feat, True)], node.mean)
    node.absent = build(absent, max_depth, min_leaf, depth + 1, predicate + [(feat, False)], node.mean)
    return node


def leaves(node):
    if node.is_leaf:
        yield node
    else:
        yield from leaves(node.present)
        yield from leaves(node.absent)


def predicate_text(predicate) -> str:
    if not predicate:
        return "(all tasks)"
    parts = []
    for feat, present in predicate:
        kind, _, value = feat.partition(":")
        label = f"{kind} {value}"
        parts.append(label if present else f"not {label}")
    return " and ".join(parts)


def slug(value: str) -> str:
    s = re.sub(r"[^a-z0-9]+", "-", value.lower()).strip("-")
    return s or "candidate"


# Configuration admission refuses these names (`other` is the fallback type),
# so a suggested block must never be labeled with one.
RESERVED_TYPE_NAMES = frozenset({"other"})


def meets_threshold(leaf, margin, min_leaf) -> bool:
    return leaf.n >= min_leaf and abs(leaf.mean - leaf.parent_mean) >= margin


def describe_feature(feat: str) -> str:
    kind, _, value = feat.partition(":")
    return f"{kind} {value}"


def unrepresentable_reason(leaf) -> str | None:
    """Why no ready-to-paste type can describe exactly this leaf's population.

    Returns None when the leaf predicate is a single positive tag/path with
    no exclusions, which the configuration matcher (first type whose tags a
    task carries any of, or whose paths match any of) expresses exactly. Any
    conjunction of positives, any exclusion, or an exclusion-only predicate
    matches a different population under any-of/no-negation semantics, so
    emitting it as an equivalent block would misdescribe the reported n/mean.
    """
    positives = [f for f, present in leaf.predicate if present]
    negatives = [f for f, present in leaf.predicate if not present]
    if len(positives) == 1 and not negatives:
        return None
    details = []
    if len(positives) > 1:
        details.append(
            "requires all of "
            + ", ".join(describe_feature(f) for f in positives)
            + " at once (conjunction)"
        )
    if negatives:
        details.append(
            "excludes " + ", ".join(describe_feature(f) for f in negatives) + " (a type has no negation)"
        )
    if not positives:
        details.append("has no positive tag or path to match on")
    return (
        "leaf predicate " + "; ".join(details) + ": a "
        "[[workflow.crew_performance_types]] entry matches a task carrying any "
        "listed tag or path, so no ready-to-paste block describes exactly this "
        "population; the reported n/mean describe the leaf only"
    )


def suggestion_for(leaf, margin, min_leaf):
    if not meets_threshold(leaf, margin, min_leaf):
        return None
    if unrepresentable_reason(leaf) is not None:
        return None
    tags = [f.partition(":")[2] for f, p in leaf.predicate if p and f.startswith("tag:")]
    paths = [f.partition(":")[2] for f, p in leaf.predicate if p and f.startswith("path:")]
    if not tags and not paths:
        return None
    name_parts = tags + paths
    name = slug("-".join(name_parts))[:64]
    if name in RESERVED_TYPE_NAMES:
        name = f"candidate-{name}"
    lines = ["[[workflow.crew_performance_types]]", f'name = "{name}"']
    if tags:
        lines.append("tags = [" + ", ".join(f'"{t}"' for t in tags) + "]")
    if paths:
        lines.append("paths = [" + ", ".join(f'"{p}/**"' for p in paths) + "]")
    return {
        "name": name,
        "tags": tags,
        "paths": [f"{p}/**" for p in paths],
        "n": leaf.n,
        "mean_relative": round(leaf.mean, 2),
        "parent_mean_relative": round(leaf.parent_mean, 2),
        "predicate": predicate_text(leaf.predicate),
        "toml": "\n".join(lines),
    }


def analyse(rows, weights, group_by, max_depth, min_leaf, margin):
    # Fleet rates per tier over every exported row.
    tier_counts: dict[str, dict[str, float]] = defaultdict(lambda: defaultdict(float))
    tier_n: dict[str, int] = defaultdict(int)
    for row in rows:
        tier = str(row.get("tier", ""))
        tier_n[tier] += 1
        for key, val in (row.get("counts") or {}).items():
            tier_counts[tier][str(key)] += float(val or 0)
    fleet_rates = {
        tier: {k: v / tier_n[tier] for k, v in counts.items()} for tier, counts in tier_counts.items()
    }
    enriched = []
    for row in rows:
        raw = task_raw(row, fleet_rates, weights)
        enriched.append((row, raw))
    crew_mean: dict[tuple[str, str, str], float] = {}
    crew_groups: dict[tuple[str, str, str], list[float]] = defaultdict(list)
    for row, raw in enriched:
        crew_groups[(str(row.get("crew", "")), str(row.get("model", "")), str(row.get("tier", "")))].append(raw)
    for key, vals in crew_groups.items():
        crew_mean[key] = sum(vals) / len(vals)

    groups: dict[tuple, list] = defaultdict(list)
    for row, raw in enriched:
        rel = raw - crew_mean[(str(row.get("crew", "")), str(row.get("model", "")), str(row.get("tier", "")))]
        if group_by == "tier":
            gkey = (str(row.get("tier", "")),)
        else:
            gkey = (str(row.get("crew", "")), str(row.get("model", "")), str(row.get("tier", "")))
        groups[gkey].append((features(row), rel, row))

    out_groups = []
    suggestions = []
    unsupported = []
    for gkey in sorted(groups):
        samples = [(feats, rel) for feats, rel, _ in groups[gkey]]
        root = build(samples, max_depth, min_leaf, 0, [], 0.0)
        group_leaves = []
        for leaf in leaves(root):
            entry = {
                "n": leaf.n,
                "mean_relative": round(leaf.mean, 2),
                "predicate": predicate_text(leaf.predicate),
            }
            group_leaves.append(entry)
            if not meets_threshold(leaf, margin, min_leaf):
                continue
            reason = unrepresentable_reason(leaf)
            if reason is not None:
                unsupported.append(
                    {
                        "group": list(gkey),
                        "n": leaf.n,
                        "mean_relative": round(leaf.mean, 2),
                        "parent_mean_relative": round(leaf.parent_mean, 2),
                        "predicate": predicate_text(leaf.predicate),
                        "reason": reason,
                    }
                )
                continue
            s = suggestion_for(leaf, margin, min_leaf)
            if s is not None:
                s = {"group": list(gkey), **s}
                suggestions.append(s)
        label = " / ".join(gkey) if group_by != "tier" else f"tier {gkey[0]}"
        out_groups.append({"group": list(gkey), "label": label, "n": len(samples), "leaves": group_leaves})
    return out_groups, suggestions, unsupported


def source_label(source: str | None) -> str:
    return "stdin" if source in (None, "-") else source


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="Suggest crew performance types from a per-task export.")
    ap.add_argument("source", nargs="?", default="-", help="Export JSON file, or - / omitted for stdin")
    ap.add_argument("--max-depth", type=int, default=3)
    ap.add_argument("--min-leaf", type=int, default=30)
    ap.add_argument("--margin", type=float, default=8.0,
                    help="Minimum |leaf mean - parent mean| in points for a suggestion (default 8)")
    ap.add_argument("--group-by", choices=["crew-tier", "tier"], default="crew-tier")
    ap.add_argument("--json", action="store_true", help="Emit JSON instead of text")
    args = ap.parse_args(argv)
    if args.min_leaf < 1 or args.max_depth < 0:
        ap.error("--min-leaf must be at least 1 and --max-depth at least 0")
    try:
        rows, weights = load_rows(args.source)
    except OSError as exc:
        detail = exc.strerror or exc
        print(f"error: cannot read export from {source_label(args.source)}: {detail}", file=sys.stderr)
        return 1
    except (json.JSONDecodeError, UnicodeDecodeError) as exc:
        print(f"error: invalid JSON in export from {source_label(args.source)}: {exc}", file=sys.stderr)
        return 1
    groups, suggestions, unsupported = analyse(
        rows, weights, args.group_by, args.max_depth, args.min_leaf, args.margin
    )
    if args.json:
        print(json.dumps({"groups": groups, "suggestions": suggestions, "unsupported": unsupported}, indent=2))
        return 0
    for group in groups:
        print(f"## {group['label']} (n={group['n']})")
        for leaf in group["leaves"]:
            print(f"  leaf n={leaf['n']} mean_relative={leaf['mean_relative']:+.2f}  when {leaf['predicate']}")
        print()
    if suggestions:
        print("Suggested types (review before adopting; this report never applies them):")
        print()
        for s in suggestions:
            print(f"# group {' / '.join(s['group'])}: n={s['n']} mean_relative={s['mean_relative']:+.2f} when {s['predicate']}")
            print(s["toml"])
            print()
    else:
        print("No suggested types: no leaf met the n and margin thresholds with an expressible predicate.")
        print()
    if unsupported:
        print("Leaves with no equivalent ready-to-paste type (analysis only; not suggestions):")
        print()
        for u in unsupported:
            print(f"# group {' / '.join(u['group'])}: n={u['n']} mean_relative={u['mean_relative']:+.2f} when {u['predicate']}")
            print(f"# not a suggestion: {u['reason']}")
            print()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

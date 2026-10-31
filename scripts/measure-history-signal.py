#!/usr/bin/env python3
"""Measure low-signal bulk across a workspace's task history surfaces.

This is the method behind [ORB-10343]. It is committed rather than run ad hoc so
the "which surface is actually the offender" question is answered with numbers
that anyone can reproduce, and so the same numbers can be taken again after a
mitigation lands.

It reports, per surface (`events.jsonl` history rows and `comments.jsonl` rows):

* bytes per entry — n, total, mean, p50, p95, max
* boilerplate ratio — JSON envelope bytes (ids, timestamps, statuses) as a
  fraction of total bytes, i.e. how much is structure rather than payload
* embedded-blob frequency — entries whose free-text payload carries a
  serialized JSON object or array rather than prose
* a by-event-type breakdown of every entry over `--blob-threshold`, which is
  what actually names the offender

Usage:

    scripts/measure-history-signal.py                     # this repo's .orbit/tasks
    scripts/measure-history-signal.py path/to/.orbit/tasks
    scripts/measure-history-signal.py --json              # machine-readable

Nothing is written and nothing is mutated; it only reads task bundles.
"""

from __future__ import annotations

import json
import math
import re
from argparse import ArgumentParser
from pathlib import Path

# A payload is "blob-shaped" when it contains a JSON object/array opener
# immediately followed by a quoted key, or repeats the `","` separator that a
# serialized string list produces. Prose that merely mentions a brace does not
# match.
BLOB_SHAPE = re.compile(r'[\[{]"')
BLOB_SEPARATOR_RUNS = 3


def percentile(values: list[int], fraction: float) -> int:
    """Nearest-rank percentile over an already-sorted list."""
    if not values:
        return 0
    index = max(0, min(math.ceil(len(values) * fraction) - 1, len(values) - 1))
    return values[index]


def summarize(sizes: list[int]) -> dict:
    if not sizes:
        return {"n": 0, "total": 0, "mean": 0, "p50": 0, "p95": 0, "max": 0}
    ordered = sorted(sizes)
    return {
        "n": len(ordered),
        "total": sum(ordered),
        "mean": round(sum(ordered) / len(ordered)),
        "p50": percentile(ordered, 0.50),
        "p95": percentile(ordered, 0.95),
        "max": ordered[-1],
    }


def is_blob_shaped(payload: str) -> bool:
    return bool(BLOB_SHAPE.search(payload)) or payload.count('","') > BLOB_SEPARATOR_RUNS


def read_rows(path: Path) -> list[tuple[str, dict | None]]:
    """Yield (raw_line, parsed_or_None) for each non-empty JSONL row.

    A trailing corrupt line is tolerated: the bundle spec allows readers to see
    an unterminated tail, and a measurement pass must not be the thing that
    fails on one.
    """
    rows: list[tuple[str, dict | None]] = []
    with path.open(encoding="utf-8", errors="replace") as handle:
        for line in handle:
            line = line.rstrip("\n")
            if not line:
                continue
            try:
                rows.append((line, json.loads(line)))
            except json.JSONDecodeError:
                rows.append((line, None))
    return rows


def measure_surface(
    tasks_root: Path, filename: str, payload_key: str, type_key: str | None, blob_threshold: int
) -> dict:
    sizes: list[int] = []
    payload_bytes = 0
    payloads = 0
    blobs = 0
    oversized: dict[str, dict] = {}

    for bundle in sorted(tasks_root.iterdir()):
        source = bundle / filename
        if not bundle.is_dir() or not source.is_file():
            continue
        for line, parsed in read_rows(source):
            size = len(line.encode("utf-8"))
            sizes.append(size)
            if not isinstance(parsed, dict):
                continue
            payload = parsed.get(payload_key)
            if not isinstance(payload, str):
                payload = ""
            if payload:
                payloads += 1
                payload_bytes += len(payload.encode("utf-8"))
                if is_blob_shaped(payload):
                    blobs += 1
            if size > blob_threshold:
                raw_label = parsed.get(type_key) if type_key else None
                label = raw_label if isinstance(raw_label, str) and raw_label else "-"
                entry = oversized.setdefault(label, {"count": 0, "bytes": 0, "examples": []})
                entry["count"] += 1
                entry["bytes"] += size
                if len(entry["examples"]) < 3:
                    entry["examples"].append({"task": bundle.name, "bytes": size})

    stats = summarize(sizes)
    total = stats["total"]
    return {
        "surface": filename,
        "entries": stats,
        "payload_bytes": payload_bytes,
        "payload_entries": payloads,
        "boilerplate_ratio": round((total - payload_bytes) / total, 3) if total else 0.0,
        "blob_shaped_payloads": blobs,
        "oversized_by_type": dict(
            sorted(oversized.items(), key=lambda item: -item[1]["bytes"])
        ),
    }


def render(report: dict, blob_threshold: int) -> str:
    lines = [f"tasks_root: {report['tasks_root']}", f"bundles: {report['bundles']}", ""]
    for surface in report["surfaces"]:
        stats = surface["entries"]
        lines.append(f"[{surface['surface']}]")
        lines.append(
            "  entries n={n} total={total} B mean={mean} p50={p50} p95={p95} max={max}".format(
                **stats
            )
        )
        lines.append(
            f"  boilerplate ratio {surface['boilerplate_ratio']} "
            f"(envelope {stats['total'] - surface['payload_bytes']} B / payload "
            f"{surface['payload_bytes']} B)"
        )
        lines.append(
            f"  blob-shaped payloads {surface['blob_shaped_payloads']}"
            f"/{surface['payload_entries']}"
        )
        if surface["oversized_by_type"]:
            lines.append(f"  entries over {blob_threshold} B, by type:")
            for label, entry in surface["oversized_by_type"].items():
                examples = ", ".join(
                    f"{item['task']}:{item['bytes']}B" for item in entry["examples"]
                )
                share = entry["bytes"] / stats["total"] if stats["total"] else 0
                lines.append(
                    f"    {label}: count={entry['count']} bytes={entry['bytes']} "
                    f"({share:.1%} of surface) [{examples}]"
                )
        else:
            lines.append(f"  no entries over {blob_threshold} B")
        lines.append("")
    return "\n".join(lines)


def main() -> int:
    repo_root = Path(__file__).resolve().parent.parent
    parser = ArgumentParser(description=__doc__)
    parser.add_argument(
        "tasks_root",
        nargs="?",
        default=repo_root / ".orbit" / "tasks",
        type=Path,
        help="task bundle root (default: this repo's .orbit/tasks)",
    )
    parser.add_argument(
        "--blob-threshold",
        type=int,
        default=2000,
        help="entry size in bytes above which an entry is broken out by type (default: 2000)",
    )
    parser.add_argument("--json", action="store_true", help="emit the raw report as JSON")
    args = parser.parse_args()

    tasks_root: Path = args.tasks_root
    if not tasks_root.is_dir():
        parser.error(f"no task bundle root at {tasks_root}")

    surfaces = [
        measure_surface(tasks_root, "events.jsonl", "note", "type", args.blob_threshold),
        measure_surface(tasks_root, "comments.jsonl", "body", None, args.blob_threshold),
    ]
    report = {
        "tasks_root": str(tasks_root),
        "bundles": sum(1 for entry in tasks_root.iterdir() if entry.is_dir()),
        "blob_threshold": args.blob_threshold,
        "surfaces": surfaces,
    }

    print(json.dumps(report, indent=2) if args.json else render(report, args.blob_threshold))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

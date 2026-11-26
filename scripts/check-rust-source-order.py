#!/usr/bin/env python3
"""Fail on Cargo compiler warnings outside the recorded STD-12 debt."""

import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / "scripts/rust-source-order-baseline.tsv"
ORDERING = "clippy::arbitrary_source_item_ordering"
LITERALS = "clippy::inconsistent_struct_constructor"
MODULE_OR_IMPORT = re.compile(r"(?:(?:pub(?:\([^)]*\))?|unsafe)\s+)*(?:mod|use|extern)\b")


def span_text(span):
    return [
        line["text"][line["highlight_start"] - 1 : line["highlight_end"] - 1].strip()
        for line in span["text"]
    ]


def debt_key(diagnostic):
    """Identify an occurrence independently of line offsets and build target."""
    code = (diagnostic.get("code") or {}).get("code")
    if code not in (ORDERING, LITERALS) or diagnostic["level"] != "warning":
        return None
    primary = next((span for span in diagnostic["spans"] if span["is_primary"]), None)
    if not primary or not primary["text"]:
        return None

    path = Path(primary["file_name"])
    if path.is_absolute():
        if not path.is_relative_to(ROOT):
            return None
        path = path.relative_to(ROOT)
    # The existing module/use lint stays strict. Local, justified expectations
    # still work because Clippy evaluates them before emitting any diagnostic.
    if code == ORDERING:
        first_line = primary["text"][0]["text"].lstrip()
        if MODULE_OR_IMPORT.match(first_line):
            return None

    # The preceding declaration for R20 or the suggested field order for R23
    # distinguishes two violations with otherwise identical primary spans.
    if code == ORDERING:
        excerpt = [
            primary["text"][0]["text"].strip(),
            [span["text"][0]["text"].strip() for child in diagnostic["children"]
             for span in child["spans"] if span["text"]],
        ]
    else:
        excerpt = [
            span_text(primary),
            [span["suggested_replacement"] for child in diagnostic["children"]
             for span in child["spans"] if span["suggested_replacement"] is not None],
        ]
    digest = hashlib.sha256(json.dumps(excerpt).encode()).hexdigest()[:24]
    kind = "declaration" if code == ORDERING else "literal"
    return (path.as_posix(), kind, digest), (primary["byte_start"], primary["byte_end"])


def read_baseline(source):
    limits = {}
    for line in source.splitlines():
        path, kind, digest, count = line.split("\t")
        key = path, kind, digest
        if (key in limits or int(count) < 1 or not path.startswith("crates/")
                or kind not in ("declaration", "literal") or not re.fullmatch(r"[0-9a-f]{24}", digest)):
            raise ValueError(f"invalid debt entry {line!r}")
        limits[key] = int(count)
    return limits


def check_baseline(limits):
    base = os.environ.get("CI_TEST_BASE")
    if not base:
        return 0

    def git(*args):
        return subprocess.check_output(["git", "-C", ROOT, *args], text=True)

    fork = git("merge-base", "--", "HEAD", base).strip()
    path = BASELINE.relative_to(ROOT).as_posix()
    if not git("ls-tree", fork, "--", path):
        return 0  # The PR introducing the baseline has no previous inventory.
    previous = read_baseline(git("show", f"{fork}:{path}"))
    additions = sorted(key for key, count in limits.items() if count > previous.get(key, 0))
    for path, kind, digest in additions:
        print(f"Rust lint gate: added {kind} debt in {path} ({digest})", file=sys.stderr)
    return int(bool(additions))


def check(command, machine_output, limits):
    counts = Counter()
    seen = set()
    failed = False
    with subprocess.Popen(command, stdout=subprocess.PIPE, text=True) as process:
        for line in process.stdout:
            if not line.startswith("{"):
                print(line, end="")
                continue
            message = json.loads(line)
            if message.get("reason") != "compiler-message":
                if machine_output:
                    print(line, end="")
                continue
            diagnostic = message["message"]
            if diagnostic["level"] == "warning":
                occurrence = debt_key(diagnostic)
                if occurrence:
                    key, location = occurrence
                    if occurrence not in seen:
                        seen.add(occurrence)
                        counts[key] += 1
                    if counts[key] <= limits.get(key, 0):
                        continue
                failed = True
            if machine_output:
                print(line, end="")
            elif diagnostic.get("rendered"):
                print(diagnostic["rendered"], end="", file=sys.stderr)
        status = process.wait()
    if failed:
        print("Rust lint gate: compiler warnings outside the source-order baseline", file=sys.stderr)
    return status or int(failed)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true", help="forward Cargo JSON")
    parser.add_argument("--check-baseline", action="store_true", help="validate and enforce the PR baseline cap")
    parser.add_argument("command", nargs="*", help="Cargo command with --message-format=json")
    args = parser.parse_args()
    try:
        limits = read_baseline(BASELINE.read_text())
        if args.check_baseline:
            return check_baseline(limits)
        if not args.command:
            parser.error("a Cargo command is required")
        return check(args.command, args.json, limits)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"Rust lint gate: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Prove that nextest's count partitions together run the whole test list.

usage: check-nextest-partitions.py --shards N [--allow-empty] -- COMMAND [ARG ...]

COMMAND is a `cargo nextest list` invocation; the CI test-build job passes the
archive the shard jobs run. This script appends `--message-format json`, runs
the command once unpartitioned and once per `--partition count:K/N`, and fails
unless the shards' selected tests are pairwise disjoint, each non-empty, and
together equal the unpartitioned selection. Ignored tests stay out of every
selection, exactly as in the run, so the comparison is between what one
unsharded run would execute and what the N shard jobs execute.

--allow-empty admits empty shards and an empty listing, for an archive of the
affected crates only: few tests can leave a count partition empty, and that
shard's job skips its run. Coverage and disjointness are still required.
"""

import argparse
import json
import subprocess
import sys


def selected(command, partition=None):
    """Return the (binary id, test name) pairs the listing selects to run."""
    argv = [*command, "--message-format", "json"]
    if partition is not None:
        argv += ["--partition", partition]
    result = subprocess.run(argv, stdout=subprocess.PIPE, text=True)
    if result.returncode != 0:
        raise SystemExit(f"nextest-partitions: {' '.join(argv)} exited {result.returncode}")
    listing = json.loads(result.stdout)
    return {
        (binary_id, name)
        for binary_id, suite in listing["rust-suites"].items()
        for name, case in suite["testcases"].items()
        if case["filter-match"]["status"] == "matches"
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--shards", type=int, required=True)
    parser.add_argument("--allow-empty", action="store_true")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if args.shards < 1 or not command:
        parser.error("need --shards N (N >= 1) and a nextest list command after --")

    full = selected(command)
    errors = []
    if not full and not args.allow_empty:
        errors.append("the unpartitioned listing selects no tests")
    seen = {}
    for shard in range(1, args.shards + 1):
        tests = selected(command, f"count:{shard}/{args.shards}")
        print(f"nextest-partitions: shard {shard}/{args.shards} selects {len(tests)} of {len(full)} tests")
        if not tests and not args.allow_empty:
            errors.append(f"shard {shard}/{args.shards} selects no tests")
        for test in sorted(tests - full):
            errors.append(f"shard {shard}/{args.shards} selects {' '.join(test)}, "
                          "which the unpartitioned listing does not")
        for test in sorted(tests & seen.keys()):
            errors.append(f"{' '.join(test)} is in shard {seen[test]} and shard {shard}")
        for test in tests:
            seen.setdefault(test, shard)
    for test in sorted(full - seen.keys()):
        errors.append(f"no shard selects {' '.join(test)}")
    if errors:
        for error in errors:
            print(f"nextest-partitions: {error}", file=sys.stderr)
        return 1
    print(f"nextest-partitions: {args.shards} shards cover all {len(full)} tests, each exactly once")
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Exercise the nextest partition guard against a fixture `cargo nextest list`."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent
TESTS = [("orbit-core::runtime", "a"), ("orbit-core::runtime", "b"), ("orbit-cli::task", "c"),
         ("orbit-cli::task", "d"), ("orbit-exec", "e")]

# Prints a nextest JSON listing. FIXTURE_LISTINGS maps "full" and each
# partition argument to the tests it selects; every other test is reported
# as a partition mismatch, and an ignored test is never selected.
LISTER = r'''#!/usr/bin/env python3
import json, os, sys
listings = json.load(open(os.environ["FIXTURE_LISTINGS"]))
args = sys.argv[1:]
with open(os.environ["FIXTURE_CALLS"], "a") as calls:
    calls.write(json.dumps(args) + "\n")
key = args[args.index("--partition") + 1] if "--partition" in args else "full"
if key not in listings:
    sys.exit(3)
selected = {tuple(test) for test in listings[key]}
suites = {}
for binary_id, name in [tuple(test) for test in listings["universe"]]:
    status = {"status": "matches"} if (binary_id, name) in selected else {"status": "mismatch", "reason": "partition"}
    suites.setdefault(binary_id, {"testcases": {}})["testcases"][name] = {"filter-match": status}
suites.setdefault("orbit-exec", {"testcases": {}})["testcases"]["ignored"] = {
    "filter-match": {"status": "mismatch", "reason": "ignored"}}
print(json.dumps({"rust-suites": suites}))
'''


class PartitionGuardTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="nextest-partitions-", dir=scratch)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.lister = self.root / "lister"
        self.lister.write_text(LISTER)
        self.lister.chmod(0o755)

    def guard(self, listings, shards=3, *options):
        listings = dict(listings, universe=TESTS)
        (self.root / "listings.json").write_text(json.dumps(listings))
        calls = self.root / "calls.jsonl"
        calls.write_text("")
        result = subprocess.run(
            [sys.executable, str(SCRIPTS / "check-nextest-partitions.py"), "--shards", str(shards), *options,
             "--", str(self.lister), "nextest", "list", "--archive-file", "fixture.tar.zst"],
            env=dict(os.environ, FIXTURE_LISTINGS=str(self.root / "listings.json"), FIXTURE_CALLS=str(calls)),
            text=True, capture_output=True)
        return result, [json.loads(line) for line in calls.read_text().splitlines()]

    def test_complete_disjoint_partitions_pass(self):
        result, calls = self.guard({"full": TESTS, "count:1/3": TESTS[0:2], "count:2/3": TESTS[2:4],
                                    "count:3/3": TESTS[4:]})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("3 shards cover all 5 tests, each exactly once", result.stdout)
        # One unpartitioned listing, then one per shard, each in JSON.
        self.assertEqual([call[call.index("--partition") + 1] if "--partition" in call else None
                          for call in calls], [None, "count:1/3", "count:2/3", "count:3/3"])
        self.assertTrue(all(call[:4] == ["nextest", "list", "--archive-file", "fixture.tar.zst"]
                            and "--message-format" in call for call in calls))

    def test_gaps_overlaps_empty_shards_and_listing_failures_fail(self):
        cases = {
            "a test no shard runs": ({"full": TESTS, "count:1/3": TESTS[0:2], "count:2/3": TESTS[2:3],
                                      "count:3/3": TESTS[4:]}, "no shard selects orbit-cli::task d"),
            "a test two shards run": ({"full": TESTS, "count:1/3": TESTS[0:3], "count:2/3": TESTS[2:4],
                                       "count:3/3": TESTS[4:]}, "orbit-cli::task c is in shard 1 and shard 2"),
            "an empty shard": ({"full": TESTS, "count:1/3": TESTS[0:3], "count:2/3": [],
                                "count:3/3": TESTS[3:]}, "shard 2/3 selects no tests"),
            "a shard beyond the full list": ({"full": TESTS[:4], "count:1/3": TESTS[0:2],
                                              "count:2/3": TESTS[2:4], "count:3/3": TESTS[4:]},
                                             "shard 3/3 selects orbit-exec e, which the unpartitioned"),
            "an empty test list": ({"full": [], "count:1/3": [], "count:2/3": [], "count:3/3": []},
                                   "the unpartitioned listing selects no tests"),
            "a failed listing": ({"full": TESTS, "count:1/3": TESTS[0:2], "count:2/3": TESTS[2:]},
                                 "exited 3"),
        }
        for name, (listings, message) in cases.items():
            with self.subTest(case=name):
                result, _calls = self.guard(listings)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn(message, result.stderr)

    def test_an_affected_crates_archive_admits_empty_shards_but_not_gaps_or_overlaps(self):
        few = TESTS[:2]
        for listings in ({"full": few, "count:1/3": few[:1], "count:2/3": few[1:], "count:3/3": []},
                         {"full": [], "count:1/3": [], "count:2/3": [], "count:3/3": []}):
            with self.subTest(tests=len(listings["full"])):
                result, _calls = self.guard(listings, 3, "--allow-empty")
                self.assertEqual(result.returncode, 0, result.stderr)
        for listings, message in (
                ({"full": few, "count:1/3": few[:1], "count:2/3": [], "count:3/3": []},
                 "no shard selects orbit-core::runtime b"),
                ({"full": few, "count:1/3": few, "count:2/3": few[1:], "count:3/3": []},
                 "orbit-core::runtime b is in shard 1 and shard 2")):
            with self.subTest(message=message):
                result, _calls = self.guard(listings, 3, "--allow-empty")
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn(message, result.stderr)


if __name__ == "__main__":
    unittest.main()

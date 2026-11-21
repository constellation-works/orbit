#!/usr/bin/env python3
"""CLI regression: paired.py must not pass parity without a successful observation.

Drives ``scripts/perf/paired.py`` with fake arm binaries. A match requires an
actual successful observation from every arm. Missing observations, failed
probes and differing observations refuse the run before a bounded verdict.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
PAIRED = REPO / "scripts" / "perf" / "paired.py"
SCRATCH = REPO / ".orbit" / "tmp"

# Role is baked in so the control copy of the base binary agrees with base,
# while the candidate binary can fail or differ on its own.
ARM_SCRIPT = """#!/bin/sh
role={role}
mode=$1
case "$mode" in
  probe-same)
    printf 'observed\\n'
    exit 0
    ;;
  probe-fail)
    exit 9
    ;;
  probe-one-fail)
    if [ "$role" = candidate ]; then
      exit 9
    fi
    printf 'observed\\n'
    exit 0
    ;;
  probe-differ)
    printf '%s\\n' "$role"
    exit 0
    ;;
  timed)
    printf 'timed %s\\n' "$role"
    exit 0
    ;;
  *)
    printf 'unexpected %s\\n' "$mode" >&2
    exit 3
    ;;
esac
"""


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_snapshot(snapshot: Path) -> None:
    """A digest-verified fixture root small enough for a CLI parity run."""
    orbit_dir = snapshot / "checkout" / ".orbit"
    orbit_dir.mkdir(parents=True)
    (orbit_dir / "config.yaml").write_text("workspace_id: test-storage\n")
    (snapshot / "home").mkdir()
    files = []
    for path in snapshot.rglob("*"):
        if not path.is_file():
            continue
        files.append({
            "path": path.relative_to(snapshot).as_posix(),
            "size": path.stat().st_size,
            "sha256": sha256_file(path),
        })
    files.sort(key=lambda entry: entry["path"])
    manifest = {
        "schemaVersion": 1,
        "kind": "orbit-perf-snapshot",
        "source": {
            "selector": "test-ws",
            "registry_id": "test-ws",
            "name": "test-ws",
            "storage_id": "test-storage",
            "global_root": "/fixture-source/home/.orbit",
            "repo_root": "/fixture-source/checkout",
            "orbit_dir": "/fixture-source/checkout/.orbit",
            "home": "/fixture-source/home",
        },
        "snapshot_epoch": 1,
        "files": files,
    }
    (snapshot / "snapshot-manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n")


def write_builds(builds: Path) -> dict[str, Path]:
    builds.mkdir()
    binaries = {}
    revisions = {}
    for role, commit in (("base", "a"), ("candidate", "c")):
        path = builds / role
        path.write_text(ARM_SCRIPT.format(role=role))
        path.chmod(0o755)
        binaries[role] = path
        revisions[role] = {
            "binary": role,
            "binary_sha256": sha256_file(path),
            "commit": commit * 40,
            "tree": ("b" if role == "base" else "d") * 40,
        }
    (builds / "provenance.json").write_text(json.dumps(
        {"schemaVersion": 1, "kind": "orbit-perf-build-pair", "revisions": revisions},
        indent=2, sort_keys=True,
    ) + "\n")
    return binaries


class PairedCliParityTest(unittest.TestCase):
    def setUp(self) -> None:
        SCRATCH.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix="paired-cli-", dir=SCRATCH)
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.builds = root / "builds"
        self.snapshot = root / "snapshot"
        self.snapshot.mkdir()
        write_snapshot(self.snapshot)
        self.binaries = write_builds(self.builds)
        self.lane_lock = root / "lane.lock"

    def run_paired(self, *extra: str) -> tuple[subprocess.CompletedProcess[str], Path]:
        out = Path(self.temp.name) / f"out-{len(extra)}-{extra[0] if extra else 'cli'}"
        # A private lock: this regression must not take the host perf lane.
        env = os.environ.copy()
        env["ORBIT_PERF_LANE_LOCK"] = str(self.lane_lock)
        result = subprocess.run(
            [sys.executable, "-I", str(PAIRED),
             "--builds", str(self.builds),
             "--snapshot", str(self.snapshot),
             "--out", str(out),
             "--rounds", "1", "--samples", "1", "--warmup", "0", "--gap-ms", "0",
             "--lane-wait", "5", "--bound-ms", "600000",
             "--cli", "timed", *extra],
            cwd=REPO,
            env=env,
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
        return result, out

    def assert_no_passing_verdict(
        self, result: subprocess.CompletedProcess[str], out: Path,
    ) -> dict:
        self.assertEqual(result.returncode, 2, result.stderr + result.stdout)
        report = json.loads((out / "parity.json").read_text())
        self.assertIs(report["match"], False)
        self.assertFalse((out / "paired-summary.json").exists(), result.stdout)
        self.assertNotIn('"verdict": "pass"', result.stdout)
        return report

    def test_omitted_parity_does_not_match_different_timed_stdout(self) -> None:
        base = subprocess.run(
            [self.binaries["base"], "timed"], capture_output=True, check=False, timeout=10)
        candidate = subprocess.run(
            [self.binaries["candidate"], "timed"], capture_output=True, check=False, timeout=10)
        self.assertEqual(base.returncode, 0)
        self.assertEqual(candidate.returncode, 0)
        self.assertNotEqual(base.stdout, candidate.stdout)

        result, out = self.run_paired()
        report = self.assert_no_passing_verdict(result, out)
        self.assertEqual(set(report["observation_failures"]), {"base", "candidate", "control"})
        self.assertTrue(all(
            reason == "missing observation"
            for reason in report["observation_failures"].values()))
        self.assertEqual(report["arms"]["base"], {})

    def test_identical_failed_probes_do_not_match(self) -> None:
        for role in ("base", "candidate"):
            probe = subprocess.run(
                [self.binaries[role], "probe-fail"], capture_output=True, check=False, timeout=10)
            self.assertEqual(probe.returncode, 9)
            self.assertEqual(probe.stdout, b"")

        result, out = self.run_paired("--parity", "probe-fail")
        report = self.assert_no_passing_verdict(result, out)
        self.assertEqual(set(report["observation_failures"]), {"base", "candidate", "control"})
        self.assertTrue(all(
            reason == "command exited 9" for reason in report["observation_failures"].values()))
        self.assertEqual(report["arms"]["candidate"]["command"]["exit"], 9)

    def test_one_arm_failed_probe_does_not_match(self) -> None:
        result, out = self.run_paired("--parity", "probe-one-fail")
        report = self.assert_no_passing_verdict(result, out)
        self.assertEqual(report["observation_failures"].get("candidate"), "command exited 9")
        self.assertNotIn("base", report["observation_failures"])
        self.assertNotIn("control", report["observation_failures"])
        self.assertIn("candidate", report["differing_arms"])

    def test_differing_successful_observations_do_not_match(self) -> None:
        result, out = self.run_paired("--parity", "probe-differ")
        report = self.assert_no_passing_verdict(result, out)
        self.assertEqual(report["observation_failures"], {})
        self.assertIn("candidate", report["differing_arms"])
        self.assertEqual(report["arms"]["base"]["command"]["exit"], 0)
        self.assertEqual(report["arms"]["candidate"]["command"]["exit"], 0)
        self.assertNotEqual(
            report["arms"]["base"]["command"]["output"],
            report["arms"]["candidate"]["command"]["output"])

    def test_identical_successful_observations_match_and_can_pass(self) -> None:
        result, out = self.run_paired("--parity", "probe-same")
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        report = json.loads((out / "parity.json").read_text())
        self.assertIs(report["match"], True)
        self.assertEqual(report["observation_failures"], {})
        self.assertEqual(report["differing_arms"], [])
        summary = json.loads((out / "paired-summary.json").read_text())
        self.assertEqual(summary["verdict"], "pass")
        self.assertIn('"verdict": "pass"', result.stdout)


if __name__ == "__main__":
    unittest.main()

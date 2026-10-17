#!/usr/bin/env python3
"""The CPU sampler reports /proc/stat utilisation and preserves command status.

Fixture counters make the percentage exact. A live sample only checks that the
production path prints one well-formed line and still runs the command.
"""

import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "cpu-utilisation.sh"
SCRATCH = ROOT / ".orbit" / "tmp"

# user nice system idle iowait irq softirq steal, then the guest fields a real
# /proc/stat line carries and this sampler ignores.
START = "cpu 1000 0 0 1000 0 0 0 0 0 0"
END = "cpu 1500 0 0 1300 200 0 0 0 0 0"
MEASURED = (
    "cpu-util: label=phase busy_pct=50.0 idle_pct=30.0 iowait_pct=20.0 "
    "elapsed_s=0 cores=4"
)
LIVE_LINE = re.compile(
    r"^cpu-util: label=live "
    r"(?:busy_pct=\d+\.\d+ idle_pct=\d+\.\d+ iowait_pct=\d+\.\d+ "
    r"elapsed_s=\d+ cores=\d+"
    r"|unavailable=(?:proc_stat|unchanged|decreased)"
    r"(?: elapsed_s=\d+ cores=\d+| elapsed_s=\d+))$"
)


def stat_text(aggregate, cores):
    lines = [aggregate]
    for index in range(cores):
        lines.append(f"cpu{index} 1 0 0 1 0 0 0 0 0 0")
    return "\n".join(lines) + "\n"


class CpuUtilisationTests(unittest.TestCase):
    def setUp(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="cpu-util-", dir=SCRATCH)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def run_sampler(self, args, env_extra=None):
        env = dict(os.environ)
        if env_extra:
            env.update(env_extra)
        return subprocess.run(
            [str(SCRIPT), *args],
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_counter_delta_reports_busy_idle_and_iowait(self):
        """Busy excludes idle and iowait, and cores counts cpuN lines only."""
        stat = self.root / "stat"
        stat.write_text(stat_text(START, 4))
        rewriter = (
            "from pathlib import Path\n"
            f"Path({str(stat)!r}).write_text({stat_text(END, 4)!r})\n"
            "print('ran')\n"
        )
        result = self.run_sampler(
            ["--label", "phase", "--", sys.executable, "-c", rewriter],
            {"CPU_UTIL_STAT_FILE": str(stat)},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "ran\n")
        self.assertEqual(result.stderr.strip(), MEASURED)

    def test_command_status_survives_the_sample(self):
        stat = self.root / "stat"
        stat.write_text(stat_text(START, 1))
        result = self.run_sampler(
            ["--label", "phase", "--", sys.executable, "-c", "raise SystemExit(9)"],
            {"CPU_UTIL_STAT_FILE": str(stat)},
        )
        self.assertEqual(result.returncode, 9, result.stderr)
        self.assertIn("unavailable=unchanged", result.stderr)
        self.assertIn("cores=1", result.stderr)

    def test_missing_stat_runs_the_command_and_reports_unavailable(self):
        missing = self.root / "missing"
        result = self.run_sampler(
            ["--label", "phase", "--", sys.executable, "-c", "print('still-ran')"],
            {"CPU_UTIL_STAT_FILE": str(missing)},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "still-ran\n")
        self.assertIn("cpu-util: label=phase unavailable=proc_stat elapsed_s=", result.stderr)

    def test_decreased_counter_is_not_a_percentage(self):
        stat = self.root / "stat"
        stat.write_text(stat_text(END, 2))
        rewriter = (
            "from pathlib import Path\n"
            f"Path({str(stat)!r}).write_text({stat_text(START, 2)!r})\n"
        )
        result = self.run_sampler(
            ["--label", "phase", "--", sys.executable, "-c", rewriter],
            {"CPU_UTIL_STAT_FILE": str(stat)},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("unavailable=decreased", result.stderr)
        self.assertNotIn("busy_pct", result.stderr)

    def test_invalid_label_does_not_run_the_command(self):
        marker = self.root / "ran"
        result = self.run_sampler(
            ["--label", "bad label", "--", sys.executable, "-c",
             f"open({str(marker)!r}, 'w').close()"],
        )
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertFalse(marker.exists())
        self.assertNotIn("cpu-util:", result.stderr)

    def test_live_proc_stat_sample_keeps_a_successful_command(self):
        """The production path, with no fixture file, still emits one line."""
        result = self.run_sampler(["--label", "live", "--", "true"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertRegex(result.stderr.strip(), LIVE_LINE)

    def test_ci_profile_raises_threads_and_keeps_group_caps(self):
        """Profile ci is the only thread change; the named group caps stay."""
        config = tomllib.loads((ROOT / ".config" / "nextest.toml").read_text())
        profile = config["profile"]["ci"]
        self.assertEqual(profile["test-threads"], 6, "CI oversubscribe is two slots on 4 vCPUs")
        self.assertEqual(profile["retries"], 0, "a flake is fixed at its cause, not rerun")
        self.assertNotIn("overrides", profile)
        self.assertNotIn("test-threads", config["profile"]["default"])
        groups = config["test-groups"]
        self.assertEqual(groups["stateful-runtime"]["max-threads"], 1)
        self.assertEqual(groups["task-contracts"]["max-threads"], 4)
        self.assertEqual(groups["mcp-contracts"]["max-threads"], 8)


if __name__ == "__main__":
    unittest.main()

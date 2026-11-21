#!/usr/bin/env python3
"""Paired base/candidate/control timing of one Orbit surface on a frozen fixture.

    paired.py --builds <builds-dir> --snapshot <snapshot-dir> --out <results-dir>
              (--cli '<orbit args>' | --http '<GET path>')
              [--parity '<orbit args>' --parity-mask <field>...]
              [--rounds 3] [--samples 10] [--warmup 1] [--gap-ms 250] [--bound-ms <n>]

Three arms: the base and candidate binaries from build-pair.sh, and a control
that is the base binary copied to a second path, so the unchanged-build spread
is measured. Every round restores a fresh root from the snapshot. Holds the
host-wide perf lane for the whole run. A match requires a successful
observation from every arm (parity command exit 0, and HTTP 2xx when that
probe runs). A missing or failed observation refuses the run before sampling.
See docs/runbooks/perf-timing-protocol.md.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import selectors
import shlex
import shutil
import signal
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import fixture  # noqa: E402
import perf_common as common  # noqa: E402

SCHEMA_VERSION = 1
ARMS = ("base", "candidate", "control")
STARTUP_TIMEOUT_SECONDS = 60.0
HTTP_TIMEOUT_SECONDS = 300.0


# ── statistics ───────────────────────────────────────────────────────────────


def nearest_rank_p95(values: list[float]) -> float:
    """Nearest-rank p95 (ORB-25638): the ceil(0.95 n)-th smallest sample."""
    ordered = sorted(values)
    rank = max(1, math.ceil(0.95 * len(ordered)))
    return ordered[rank - 1]


def round_order(rounds: int) -> list[str]:
    """Balanced interleaving: forward, then reversed, alternating per round.

    With two arms over three rounds this is ORB-25652's A B B A A B.
    """
    order: list[str] = []
    for index in range(rounds):
        order.extend(ARMS if index % 2 == 0 else tuple(reversed(ARMS)))
    return order


def verdict(candidate: float, spread: float, bound: float | None) -> dict:
    if bound is None:
        return {"verdict": "informational", "reason": "no --bound-ms given"}
    if candidate > bound:
        return {"verdict": "fail", "reason": f"candidate median {candidate:.1f} ms > bound {bound:g} ms"}
    if candidate <= bound - spread:
        return {"verdict": "pass",
                "reason": f"candidate median {candidate:.1f} ms <= bound {bound:g} - control spread {spread:.1f} ms"}
    return {"verdict": "inconclusive",
            "reason": (f"candidate median {candidate:.1f} ms is within the control spread "
                       f"{spread:.1f} ms of the bound {bound:g} ms; rerun with more --rounds")}


# ── host pressure and network ────────────────────────────────────────────────


def orbit_leaf_count() -> int:
    """Processes carrying Orbit's managed-run marker, outside this harness."""
    own = {os.getpid(), os.getppid()}
    count = 0
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit() or int(entry.name) in own:
            continue
        try:
            environ = (entry / "environ").read_bytes()
        except OSError:
            continue
        if b"ORBIT_MANAGED_RUN_CONTEXT=" in environ:
            count += 1
    return count


def host_pressure() -> dict:
    try:
        loadavg = Path("/proc/loadavg").read_text().strip()
    except OSError:
        loadavg = ""
    return {"at": common.now_iso(), "loadavg": loadavg, "orbit_leaf_processes": orbit_leaf_count()}


def network_isolation() -> tuple[list[str], dict]:
    """A prefix that runs a command without network, when the host allows it."""
    for prefix in (["unshare", "--map-current-user", "--net"], ["unshare", "-rn"]):
        try:
            probe = subprocess.run([*prefix, "true"], capture_output=True, text=True, timeout=10)
        except (OSError, subprocess.TimeoutExpired) as error:
            last = str(error)
            continue
        if probe.returncode == 0:
            return prefix, {"isolated": True, "method": " ".join(prefix)}
        last = probe.stderr.strip()
    return [], {"isolated": False, "reason": f"unshare unavailable: {last}"}


# ── processes ────────────────────────────────────────────────────────────────


class Server:
    """One arm's `orbit web serve` on its restored root (after ORB-25672)."""

    def __init__(self, binary: Path, info: dict, env: dict):
        self.process = subprocess.Popen(
            [str(binary), "web", "serve", "--no-open", "--port", "0",
             "--workspace", info["workspace"]],
            cwd=info["cwd"], env=env,
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        try:
            line = self._announcement(STARTUP_TIMEOUT_SECONDS)
            match = re.search(r"http://(\S+)", line)
            if not match:
                raise RuntimeError(f"web serve did not announce its address: {line!r}")
            self.base = "http://" + match.group(1).rstrip("/")
            self._wait_ready()
        except BaseException:
            self.stop()
            raise

    def _announcement(self, timeout: float) -> str:
        deadline = time.monotonic() + timeout
        pending = bytearray()
        with selectors.DefaultSelector() as selector:
            selector.register(self.process.stdout, selectors.EVENT_READ)
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise RuntimeError("web serve did not announce its address in time")
                if selector.select(min(remaining, 0.1)):
                    chunk = os.read(self.process.stdout.fileno(), 4096)
                    pending.extend(chunk)
                    if b"\n" in pending:
                        return pending.split(b"\n", 1)[0].decode("utf-8", "replace")
                    if not chunk:
                        raise RuntimeError("web serve closed stdout before announcing")
                if self._leader_exited():
                    raise RuntimeError("web serve exited before announcing its address")

    def _wait_ready(self) -> None:
        deadline = time.monotonic() + STARTUP_TIMEOUT_SECONDS
        while True:
            try:
                with urllib.request.urlopen(self.base + "/", timeout=10) as response:
                    response.read()
                    return
            except urllib.error.HTTPError:
                return  # the server answered
            except (urllib.error.URLError, OSError):
                if time.monotonic() >= deadline:
                    raise RuntimeError("web serve never answered on its address")
                time.sleep(0.1)

    def get(self, path: str) -> tuple[int, bytes]:
        try:
            with urllib.request.urlopen(self.base + path, timeout=HTTP_TIMEOUT_SECONDS) as response:
                return response.status, response.read()
        except urllib.error.HTTPError as error:
            return error.code, error.read()

    def _leader_exited(self) -> bool:
        """Whether the leader exited, without reaping it.

        `WNOWAIT` leaves an exited leader as a zombie, which pins its process
        group id until `stop` reaps it, so no group signal can land on a
        reused id (STD-03 §R14).
        """
        try:
            info = os.waitid(os.P_PID, self.process.pid,
                             os.WEXITED | os.WNOHANG | os.WNOWAIT)
        except ChildProcessError:
            return True
        return info is not None

    def stop(self) -> None:
        if getattr(self, "_stopped", False):
            return
        self._stopped = True
        try:
            try:
                os.killpg(self.process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            deadline = time.monotonic() + 10
            while not self._leader_exited() and time.monotonic() < deadline:
                time.sleep(0.05)
            # The leader is still unreaped, so the group id is still ours:
            # this also ends any child that outlived the leader.
            try:
                os.killpg(self.process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        finally:
            self.process.wait(timeout=10)
            if self.process.stdout is not None:
                self.process.stdout.close()


# ── parity ───────────────────────────────────────────────────────────────────


def mask_value(value, masks: list[str], path: str = ""):
    if isinstance(value, dict):
        return {
            key: mask_value(item, masks, f"{path}.{key}" if path else key)
            for key, item in sorted(value.items())
            if key not in masks and (f"{path}.{key}" if path else key) not in masks
        }
    if isinstance(value, list):
        return [mask_value(item, masks, path) for item in value]
    return value


def normalize_output(raw: bytes, masks: list[str], root: str) -> dict:
    text = raw.decode("utf-8", "replace").replace(root, "<root>")
    try:
        parsed = json.loads(text)
    except ValueError:
        return {"format": "text", "value": text}
    return {"format": "json", "value": mask_value(parsed, masks)}


def arm_observation_failure(record: dict) -> str | None:
    """Why `record` is not a successful observation, or None when it is.

    A match needs one from every arm. CLI mode with no `--parity` stores an
    empty record: that is a missing observation, not an agreement. A parity
    command must exit 0, and an HTTP probe must return a 2xx status. Identical
    non-zero exits are a failed probe even when stdout is empty on every arm.
    """
    if "command" not in record and "http" not in record:
        return "missing observation"
    if "command" in record and record["command"].get("exit") != 0:
        return f"command exited {record['command'].get('exit')}"
    if "http" in record:
        status = record["http"].get("status")
        if not isinstance(status, int) or status < 200 or status >= 300:
            return f"http status {status}"
    return None


def parity_refusal_detail(parity: dict) -> str:
    """The parenthetical of a parity refusal, covering failures and differences."""
    parts: list[str] = []
    failures = parity.get("observation_failures") or {}
    if failures:
        rendered = ", ".join(f"{arm}: {failures[arm]}" for arm in ARMS if arm in failures)
        parts.append(f"no successful observation ({rendered})")
    if parity["differing_arms"]:
        parts.append(f"{', '.join(parity['differing_arms'])} differ from base")
    return "; ".join(parts) if parts else "match is false"


# ── run ──────────────────────────────────────────────────────────────────────


class Harness:
    def __init__(self, args):
        self.args = args
        self.builds = Path(args.builds).absolute()
        self.snapshot = Path(args.snapshot).absolute()
        self.out = Path(args.out).absolute()
        self.masks = list(args.parity_mask or [])
        self.cli_args = shlex.split(args.cli) if args.cli else None
        self.http_path = args.http

    def prepare(self, created: common.CreatedDirs) -> None:
        provenance = json.loads((self.builds / "provenance.json").read_text())
        self.provenance = provenance
        self.binaries: dict[str, Path] = {}
        for role in ("base", "candidate"):
            entry = provenance["revisions"][role]
            path = self.builds / entry["binary"]
            if common.sha256_file(path) != entry["binary_sha256"]:
                raise common.PerfRefusal(f"{path} does not match provenance.json sha256")
            self.binaries[role] = path
        self.manifest = fixture.verify_snapshot(self.snapshot)
        common.require_absent_or_empty(self.out)
        created.create(self.out, output=True)
        self.work = created.create(self.out / ".work")
        control = self.work / "control" / "orbit"
        control.parent.mkdir()
        shutil.copy2(self.binaries["base"], control)
        self.binaries["control"] = control
        self.binary_sha = {arm: common.sha256_file(path) for arm, path in self.binaries.items()}
        if self.binary_sha["control"] != self.binary_sha["base"]:
            raise common.PerfRefusal("control copy differs from the base binary")
        if self.cli_args is not None:
            self.net_prefix, self.network = network_isolation()
        else:
            self.net_prefix, self.network = [], {
                "isolated": False,
                "reason": "HTTP mode: the timed server listens on host loopback for the "
                          "harness client, so it stays in the host network namespace",
            }

    def restore(self, label: str) -> dict:
        return fixture.restore_root(self.snapshot, self.work / f"root-{label}", self.manifest)

    def drop(self, info: dict) -> None:
        if not self.args.keep:
            fixture.clean_root(Path(info["root"]))

    def cli_once(self, arm: str, info: dict, env: dict, argv: list[str]) -> tuple[float, int, bytes]:
        started = time.perf_counter()
        result = subprocess.run(
            [*self.net_prefix, str(self.binaries[arm]), *argv],
            cwd=info["cwd"], env=env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        )
        return (time.perf_counter() - started) * 1000, result.returncode, result.stdout

    def parity(self) -> dict:
        arms = {}
        for arm in ARMS:
            info = self.restore(f"parity-{arm}")
            env = fixture.isolated_env(info)
            record: dict = {}
            try:
                if self.args.parity:
                    _ms, code, stdout = self.cli_once(arm, info, env, shlex.split(self.args.parity))
                    record["command"] = {"exit": code,
                                         "output": normalize_output(stdout, self.masks, info["root"])}
                if self.http_path:
                    server = Server(self.binaries[arm], info, env)
                    try:
                        status, body = server.get(self.http_path)
                    finally:
                        server.stop()
                    record["http"] = {"status": status,
                                      "body": normalize_output(body, self.masks, info["root"])}
            finally:
                self.drop(info)
            arms[arm] = record
        failures = {}
        for arm in ARMS:
            reason = arm_observation_failure(arms[arm])
            if reason:
                failures[arm] = reason
        reference = json.dumps(arms["base"], sort_keys=True)
        differing = [arm for arm in ARMS if json.dumps(arms[arm], sort_keys=True) != reference]
        # Identical failures compare equal, so content equality alone is not a match.
        return {
            "schemaVersion": SCHEMA_VERSION,
            "kind": "orbit-perf-parity",
            "command": self.args.parity,
            "http_path": self.http_path,
            "masks": self.masks,
            "match": not failures and not differing,
            "differing_arms": differing,
            "observation_failures": failures,
            "arms": arms,
        }

    def sample_round(self, arm: str, round_index: int) -> tuple[list[dict], dict]:
        info = self.restore(f"{arm}-{round_index}")
        env = fixture.isolated_env(info)
        before = host_pressure()
        samples: list[dict] = []
        gap = self.args.gap_ms / 1000
        try:
            if self.cli_args is not None:
                timed = lambda: self.cli_once(arm, info, env, self.cli_args)[:2]  # noqa: E731
                server = None
            else:
                server = Server(self.binaries[arm], info, env)

                def timed():
                    started = time.perf_counter()
                    status, _body = server.get(self.http_path)
                    ms = (time.perf_counter() - started) * 1000
                    return ms, 0 if 200 <= status < 300 else status
            try:
                total = self.args.warmup + self.args.samples
                for index in range(total):
                    if index:
                        time.sleep(gap)
                    ms, code = timed()
                    warm = index < self.args.warmup
                    samples.append({"arm": arm, "round": round_index, "index": index,
                                    "warmup": warm, "ms": round(ms, 3), "exit": code})
                    if code != 0:
                        raise common.PerfRefusal(
                            f"{arm} round {round_index} sample {index} exited {code}; failing the run")
            finally:
                if server is not None:
                    server.stop()
        finally:
            after = host_pressure()
            self.drop(info)
        return samples, {"arm": arm, "round": round_index, "before": before, "after": after}

    def run(self) -> int:
        parity = self.parity()
        common.write_json(self.out / "parity.json", parity)
        if not parity["match"]:
            raise common.PerfRefusal(
                "parity mismatch between arms "
                f"({parity_refusal_detail(parity)}); refusing to sample. "
                f"See {self.out / 'parity.json'}")
        order = round_order(self.args.rounds)
        seen = {arm: 0 for arm in ARMS}
        raw: list[dict] = []
        pressure: list[dict] = []
        for arm in order:
            samples, reading = self.sample_round(arm, seen[arm])
            seen[arm] += 1
            raw.extend(samples)
            pressure.append(reading)
        round_p95: dict[str, list[float]] = {arm: [] for arm in ARMS}
        for arm in ARMS:
            for index in range(self.args.rounds):
                values = [s["ms"] for s in raw
                          if s["arm"] == arm and s["round"] == index and not s["warmup"]]
                round_p95[arm].append(nearest_rank_p95(values))
        medians = {arm: statistics.median(values) for arm, values in round_p95.items()}
        control_range = max(round_p95["control"]) - min(round_p95["control"])
        control_offset = abs(medians["control"] - medians["base"])
        spread = max(control_range, control_offset)
        outcome = verdict(medians["candidate"], spread, self.args.bound_ms)
        surface = {"mode": "cli", "args": self.args.cli} if self.cli_args is not None \
            else {"mode": "http", "path": self.http_path}
        common.write_json(self.out / "raw.json", {
            "schemaVersion": SCHEMA_VERSION,
            "kind": "orbit-perf-raw",
            "surface": surface,
            "samples": raw,
        })
        summary = {
            "schemaVersion": SCHEMA_VERSION,
            "kind": "orbit-perf-paired-summary",
            "surface": surface,
            "protocol": {"rounds": self.args.rounds, "samples": self.args.samples,
                         "warmup": self.args.warmup, "gap_ms": self.args.gap_ms,
                         "aggregation": "per-round nearest-rank p95; per-arm median of round p95s"},
            "round_order": order,
            "revisions": {role: {k: self.provenance["revisions"][role][k]
                                 for k in ("commit", "tree", "binary_sha256")}
                          for role in ("base", "candidate")},
            "binary_sha256": self.binary_sha,
            "snapshot": {"path": str(self.snapshot),
                         "snapshot_epoch": self.manifest["snapshot_epoch"],
                         "source_workspace": self.manifest["source"]["registry_id"]},
            "round_p95_ms": round_p95,
            "median_p95_ms": medians,
            "control_spread_ms": {"value": spread, "control_range": control_range,
                                  "control_vs_base": control_offset},
            "candidate_minus_base_ms": medians["candidate"] - medians["base"],
            "bound_ms": self.args.bound_ms,
            **outcome,
            "network": self.network,
            "host_pressure": pressure,
            "lane": self.lane_holder,
            **common.host_identity(),
        }
        common.write_json(self.out / "paired-summary.json", summary)
        print(json.dumps({"verdict": summary["verdict"], "median_p95_ms": medians,
                          "control_spread_ms": spread, "results": str(self.out)}))
        return 0


def positive(value: str) -> int:
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be at least 1")
    return number


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--builds", required=True, help="build-pair.sh output directory")
    parser.add_argument("--snapshot", required=True, help="fixture.py snapshot directory")
    parser.add_argument("--out", required=True, help="new results directory")
    surface = parser.add_mutually_exclusive_group(required=True)
    surface.add_argument("--cli", help="orbit arguments to time, e.g. 'doctor --json'")
    surface.add_argument("--http", help="GET path to time on each arm's own web serve")
    parser.add_argument(
        "--parity",
        help="orbit arguments run once per arm before sampling; a CLI match needs "
             "this command to exit 0 on every arm")
    parser.add_argument("--parity-mask", nargs="+", default=[],
                        help="JSON field names (or dotted paths) removed before comparing")
    parser.add_argument("--rounds", type=positive, default=3)
    parser.add_argument("--samples", type=positive, default=10)
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument("--gap-ms", type=int, default=250)
    parser.add_argument("--bound-ms", type=float)
    parser.add_argument("--lane-wait", type=float,
                        default=float(os.environ.get("ORBIT_PERF_LANE_WAIT_SECONDS",
                                                     common.DEFAULT_LANE_WAIT_SECONDS)))
    parser.add_argument("--keep", action="store_true", help="keep restored roots and work dirs")
    args = parser.parse_args(argv)
    if args.http and not args.http.startswith("/"):
        parser.error("--http takes a path starting with /")
    os.umask(0o077)
    created = common.CreatedDirs(args.keep)
    ok = False
    try:
        with common.Lane("paired.py", args.lane_wait) as lane:
            harness = Harness(args)
            harness.lane_holder = lane.holder
            harness.prepare(created)
            status = harness.run()
            ok = True
            return status
    except common.PerfRefusal as refusal:
        print(f"paired.py: {refusal}", file=sys.stderr)
        return 2
    finally:
        work = Path(args.out).absolute() / ".work"
        if ok and not args.keep and work in created.paths:
            created.remove(work)
        # A refused run keeps parity.json for diagnosis; other partial output goes.
        keep_partial = (Path(args.out).absolute() / "parity.json").exists()
        if not ok and keep_partial and not args.keep:
            created.remove(work)
            created.keep_on_success.add(Path(args.out).absolute())
            created.finish(True)
        else:
            created.finish(ok)


if __name__ == "__main__":
    sys.exit(main())

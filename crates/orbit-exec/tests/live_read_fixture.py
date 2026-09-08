#!/usr/bin/env python3
"""Reproducible host/worker handoff. Never loads policy or retries unconfined.

prepare ROOT emits exact operator profile commands. run ROOT executes separate
baseline, Landlock and AppArmor experiments and records paired negative controls.
Exit 1 means a measured contract failure; exit 2 means missing evidence/capability.
"""

import argparse
import json
import os
from pathlib import Path
import platform
import shlex
import subprocess
import sys

import live_read_probe as probe


NEGATIVES = ["outside", "existing_denied", "create_denied", "rename_denied",
             "symlink_outside", "hardlink_denied", "descendant_outside", "preexisting_alias"]


def paired_controls(baseline, candidate):
    controls = {}
    for name in NEGATIVES:
        original = baseline.get("probes", {}).get(name, {})
        tested = candidate.get("probes", {}).get(name, {})
        exercised = (original.get("exit_code") == 0 and
                     probe.MARKER in original.get("stdout", ""))
        controls[name] = tested.get("status", "unavailable") if exercised else "unavailable"
    return controls


def command(argv):
    try:
        output = subprocess.run(argv, capture_output=True, text=True, timeout=900,
                                env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"})
        return {"argv": argv, "exit_code": output.returncode,
                "stdout": output.stdout, "stderr": output.stderr}
    except (OSError, subprocess.TimeoutExpired) as error:
        return {"argv": argv, "status": "unavailable", "reason": str(error)}


def run(root, execution_context):
    report = {"schemaVersion": 1, "execution_context": execution_context,
              "kernel": platform.release(), "uid": os.getuid(), "runs": {}}
    try:
        report["parent_apparmor_label"] = Path("/proc/self/attr/current").read_text().strip()
    except OSError as error:
        report["parent_apparmor_label"] = {"status": "unavailable", "reason": str(error)}
    report["namespace_admission_only"] = command([
        "/usr/bin/bwrap", "--die-with-parent", "--new-session", "--unshare-all",
        "--ro-bind", "/", "/", "--", "/bin/true"])
    report["fuse_device_exists"] = Path("/dev/fuse").exists()
    script = str(Path(probe.__file__).resolve())
    for backend in ["baseline", "landlock", "apparmor"]:
        execution = command([sys.executable, script, "run", str(root),
                             "--backend", backend, "--local-recovery"])
        try:
            execution["evidence"] = json.loads(execution.get("stdout", ""))
        except ValueError:
            execution.update(status="unavailable", reason="collector produced no valid JSON")
        report["runs"][backend] = execution
    baseline = report["runs"]["baseline"].get("evidence", {})
    report["paired_negative_controls"] = {
        backend: paired_controls(baseline, report["runs"][backend].get("evidence", {}))
        for backend in ["landlock", "apparmor"]}
    report["complete_contract_proven"] = False
    report["remaining_gates"] = [
        "live pathname/race evidence", "descriptor and alias contract decision",
        "production policy equivalence and admission", "supported-platform and performance evidence"]
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=["prepare", "run"])
    parser.add_argument("root", type=Path)
    parser.add_argument("--execution-context", choices=["admitted-worker", "operator-host"],
                        required=True, help="operator assertion, not automatically verified")
    args = parser.parse_args()
    root = Path(probe.safe_path(args.root))
    if args.operation == "prepare":
        manifest = probe.prepare(root)
        profile = str(root / "profile.apparmor")
        result = {"manifest": manifest, "operator_commands": {
            "compile_only": shlex.join(["apparmor_parser", "--skip-kernel-load", "--skip-cache", profile]),
            "load_requires_operator_authorization": shlex.join(["apparmor_parser", "--add", "--skip-cache", profile]),
            "test": shlex.join([sys.executable, str(Path(__file__).resolve()), "run", str(root),
                                "--execution-context", args.execution_context]),
            "remove_after_children_exit": shlex.join(["apparmor_parser", "--remove", profile])}}
        print(json.dumps(result, indent=2))
        return 0
    result = run(root, args.execution_context)
    print(json.dumps(result, indent=2))
    failures = any(item.get("evidence", {}).get("counts", {}).get("fail", 0)
                   for item in result["runs"].values())
    return 1 if failures else 2  # Remaining gates prevent a whole-contract pass.


if __name__ == "__main__":
    sys.exit(main())

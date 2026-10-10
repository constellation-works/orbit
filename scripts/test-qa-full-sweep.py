#!/usr/bin/env python3
"""Inventory guard and isolated trial runner for qa-full-sweep."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import runpy
import select
import signal
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

validate_npm_evidence = runpy.run_path(
    str(Path(__file__).with_name("check_npm_package.py")))["validate_evidence"]


# The supervisor retains process-group ownership after the command exits. We
# sweep its group before reaping it, avoiding a PID/group reuse window (STD-03 R14).
_PROCESS_SUPERVISOR = """import os, signal, subprocess, sys, threading
fd = int(sys.argv[1])
owner_fd = int(sys.argv[2])
signal.signal(signal.SIGTERM, lambda *_: None)
def watch_owner():
    os.read(owner_fd, 1)
    os.killpg(os.getpgrp(), signal.SIGKILL)
threading.Thread(target=watch_owner, daemon=True).start()
try:
    code = subprocess.call(sys.argv[3:])
except OSError as error:
    print(str(error), file=sys.stderr, flush=True)
    code = 127
os.write(fd, str(code).encode())
os.close(fd)
while True:
    signal.pause()
"""
_OUTPUT_LIMIT = 1_048_576


def run(argv, *, cwd, env, timeout=180, input_text=None):
    started = datetime.now(timezone.utc).isoformat()
    evidence = {"command": argv, "started_at": started, "exit_code": None,
                "stdout": "", "stderr": "", "outcome": "FAIL", "output_truncated": False,
                "cleanup_verified": False}
    if os.name != "posix":
        evidence["stderr"] = "QA process-group supervision requires a POSIX host"
        return evidence
    read_fd, write_fd = os.pipe()
    owner_read_fd, owner_write_fd = os.pipe()
    process = None
    threads = []
    tails = {"stdout": bytearray(), "stderr": bytearray()}
    truncated = {"stdout": False, "stderr": False}

    def terminate_group():
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            return
        except OSError as error:
            evidence["stderr"] += f"\npolite process-group termination failed: {error}"
            return
        time.sleep(.25)

    def drain(stream, name):
        try:
            while chunk := stream.read(65_536):
                tails[name].extend(chunk)
                if len(tails[name]) > _OUTPUT_LIMIT:
                    del tails[name][:-_OUTPUT_LIMIT]
                    truncated[name] = True
        finally:
            stream.close()

    def send_input(stream):
        try:
            stream.write(input_text.encode())
        except BrokenPipeError:
            pass
        finally:
            try:
                stream.close()
            except BrokenPipeError:
                pass

    try:
        process = subprocess.Popen(
            [sys.executable, "-c", _PROCESS_SUPERVISOR, str(write_fd), str(owner_read_fd), *argv],
            cwd=cwd, env=env, start_new_session=True, pass_fds=(write_fd, owner_read_fd),
            stdin=subprocess.PIPE if input_text is not None else subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        os.close(write_fd)
        write_fd = None
        os.close(owner_read_fd)
        owner_read_fd = None
        for name in tails:
            thread = threading.Thread(target=drain, args=(getattr(process, name), name), daemon=True)
            thread.start()
            threads.append(thread)
        if input_text is not None:
            thread = threading.Thread(target=send_input, args=(process.stdin,), daemon=True)
            thread.start()
            threads.append(thread)
        ready, _, _ = select.select([read_fd], [], [], timeout)
        if ready:
            code = os.read(read_fd, 64)
            if code:
                evidence["exit_code"] = int(code)
            else:
                evidence["stderr"] = "QA supervisor exited without a command result"
        else:
            evidence["stderr"] = f"timeout after {timeout}s"
            # Do not wait/reap before signalling: the live or zombie supervisor
            # reserves its own process-group identity throughout cleanup.
            terminate_group()
    except (OSError, ValueError) as error:
        evidence["stderr"] = str(error)
        if process is not None:
            terminate_group()
    except BaseException:
        if process is not None:
            terminate_group()
        raise
    finally:
        if write_fd is not None:
            os.close(write_fd)
        if owner_read_fd is not None:
            os.close(owner_read_fd)
        os.close(read_fd)
        if process is not None:
            swept = False
            try:
                os.killpg(process.pid, signal.SIGKILL)
                swept = True
            except ProcessLookupError:
                swept = True
            except OSError as error:
                evidence["exit_code"] = None
                evidence["stderr"] += f"\nprocess-group cleanup failed: {error}"
                # Closing the owner pipe also asks the supervisor to sweep its
                # own group, if the host refuses the outer process's signal.
                os.close(owner_write_fd)
                owner_write_fd = None
            try:
                process.wait(timeout=5)
                evidence["cleanup_verified"] = swept
            except subprocess.TimeoutExpired:
                evidence["exit_code"] = None
                evidence["stderr"] += "\nsupervisor reap deadline expired; descendant cleanup is unverified"
                # Reap the known supervisor only as a final bounded fallback.
                # A refused group sweep remains an unknown cleanup outcome.
                try:
                    process.kill()
                except OSError as error:
                    evidence["stderr"] += f"\nsupervisor direct kill failed: {error}"
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    evidence["stderr"] += "\nsupervisor still could not be reaped"
        if owner_write_fd is not None:
            os.close(owner_write_fd)
        for thread in threads:
            thread.join(timeout=2)
        if any(thread.is_alive() for thread in threads):
            evidence["exit_code"] = None
            evidence["cleanup_verified"] = False
            evidence["stderr"] += "\ncommand descendant retained an output pipe after group cleanup"
        evidence["stdout"] = tails["stdout"].decode(errors="replace")
        captured_stderr = tails["stderr"].decode(errors="replace")
        evidence["stderr"] = captured_stderr + ("\n" if captured_stderr and evidence["stderr"] else "") + evidence["stderr"]
        evidence["output_truncated"] = any(truncated.values())
    evidence["outcome"] = "PASS" if evidence["exit_code"] == 0 else "FAIL"
    return evidence


def source_contracts(repo: Path):
    command_source = (repo / "crates/orbit-cli/src/command/mod.rs").read_text()
    body = command_source.split("pub enum Commands {", 1)[1].split("}\n\n#[cfg(test)]", 1)[0]
    cli = set()
    pending_name = None
    for line in body.splitlines():
        named = re.search(r'name = "([^"]+)"', line)
        if named:
            pending_name = named.group(1)
            continue
        variant = re.match(r"\s*([A-Z][A-Za-z0-9]*)\s*\(", line)
        if variant:
            name = pending_name or re.sub(r"(?<!^)(?=[A-Z])", "-", variant.group(1)).lower()
            cli.add(name)
            pending_name = None

    jobs = {path.stem for path in (repo / "crates/orbit-core/assets/jobs").glob("*.yaml")}
    activities = {
        path.stem for path in (repo / "crates/orbit-core/assets/activities").glob("*.yaml")
    }
    mcp_snapshot = json.loads((repo / "crates/orbit-cli/tests/snapshots/mcp_tools_list.json").read_text())
    mcp = {entry["name"] for entry in mcp_snapshot}
    api_source = (repo / "crates/orbit-web/src/api/routes.rs").read_text()
    api = set(re.findall(r'\.route\(\s*"([^"]+)"', api_source))
    web_source = (repo / "crates/orbit-web/src/serve.rs").read_text()
    web_source += (repo / "crates/orbit-web/src/assets.rs").read_text()
    dashboard = set(re.findall(r'\.route\(\s*"([^"]+)"', web_source))
    # Embedded dashboard files are routed from the `DASHBOARD_FILES` table; a
    # body is an embedded file or a joined constant such as `DASHBOARD_CSS`.
    dashboard |= set(re.findall(
        r'\(\s*"(/[^"]*)",\s*\w+,\s*(?:include_bytes!|[A-Z][A-Z0-9_]*\.as_bytes\(\))', web_source))
    return {"cli": cli, "job": jobs, "activity": activities,
            "mcp": mcp, "api": api, "dashboard": dashboard}


def validate_inventory(repo: Path, inventory: dict):
    actual = source_contracts(repo)
    expected_cli = set(inventory["surface_contracts"]["cli_top_level"])
    expected_jobs = set(inventory["surface_contracts"]["job_assets"])
    errors = []
    ids = [item["id"] for item in inventory["scenarios"]]
    if len(ids) != len(set(ids)):
        errors.append("scenario IDs must be unique")
    if not any(item.get("required") for item in inventory["scenarios"]):
        errors.append("inventory has no required scenarios")
    if actual["cli"] != expected_cli:
        errors.append(f"CLI drift added={sorted(actual['cli']-expected_cli)} removed={sorted(expected_cli-actual['cli'])}")
    if actual["job"] != expected_jobs:
        errors.append(f"job drift added={sorted(actual['job']-expected_jobs)} removed={sorted(expected_jobs-actual['job'])}")
    for kind, expected_hash in inventory["surface_contracts"]["sha256"].items():
        observed_hash = hashlib.sha256(("\n".join(sorted(actual[kind])) + "\n").encode()).hexdigest()
        if observed_hash != expected_hash:
            errors.append(f"{kind} surface drift: update its reviewed mapping and digest")

    scenario_ids = {item["id"] for item in inventory["scenarios"]}
    claims = {}
    for family in inventory["feature_families"]:
        missing = set(family["scenarios"]) - scenario_ids
        if missing:
            errors.append(f"family {family['id']} names missing scenarios {sorted(missing)}")
    gaps = inventory.get("required_capability_gaps", {})
    for scenario in inventory["scenarios"]:
        if not scenario.get("assertions"):
            errors.append(f"scenario {scenario['id']} has no concrete assertions")
        if scenario.get("kind") == "coverage-gap":
            reason = scenario.get("coverage_gap")
            if not isinstance(reason, str) or not reason.strip():
                errors.append(f"scenario {scenario['id']} has an unexplained coverage gap")
            if "command" in scenario or "required_tests" in scenario:
                errors.append(f"scenario {scenario['id']} claims executable coverage for a gap")
        elif "coverage_gap" in scenario:
            errors.append(f"scenario {scenario['id']} must declare kind coverage-gap")
        if is_cargo_test(scenario.get("command", [])) and "required_tests" not in scenario:
            errors.append(f"scenario {scenario['id']} has no required behavioral cases")
        if "required_tests" in scenario:
            tests = scenario["required_tests"]
            if (not isinstance(tests, list) or not tests
                    or any(not isinstance(name, str) or not name.strip() for name in tests)
                    or len(tests) != len(set(tests))):
                errors.append(f"scenario {scenario['id']} has invalid required behavioral cases")
        for surface in scenario.get("surfaces", []):
            claims.setdefault(surface, []).append(scenario["id"])
    for surface, reason in gaps.items():
        if not isinstance(reason, str) or not reason.strip():
            errors.append(f"capability gap {surface} has no explanation")
    for kind, entries in actual.items():
        for entry in entries:
            surface = f"{kind}:{entry}"
            if surface not in claims and surface not in gaps:
                errors.append(f"unmapped surface {surface}")
            if surface in claims and surface in gaps:
                errors.append(f"surface {surface} is both claimed and a capability gap")
    known = {f"{kind}:{entry}" for kind, entries in actual.items() for entry in entries}
    for surface in set(claims) | set(gaps):
        if surface not in known:
            errors.append(f"coverage names absent surface {surface}")
    required_behaviors = {"normal", "failure"}
    for scenario in inventory["scenarios"]:
        if not required_behaviors.issubset(set(scenario["behavior"])):
            errors.append(f"scenario {scenario['id']} lacks normal/failure coverage")
    return errors, {kind: sorted(entries) for kind, entries in actual.items()}


def is_cargo_test(command):
    return len(command) >= 2 and Path(command[0]).name == "cargo" and command[1] == "test"


def validate_cargo_target(command, packages):
    """Check the inventory's explicit package/target against Cargo metadata."""
    try:
        package = command[command.index("-p") + 1]
        targets = packages[package]
        if "--test" in command:
            target = command[command.index("--test") + 1]
            if not any(item["name"] == target and "test" in item["kind"] for item in targets):
                raise ValueError(f"no test target {target!r} in {package}")
        elif "--lib" in command:
            if not any("lib" in item["kind"] and item.get("test", True) for item in targets):
                raise ValueError(f"no testable library in {package}")
        else:
            raise ValueError("Cargo scenario must select an explicit --test target or --lib")
    except (IndexError, KeyError) as error:
        raise ValueError("Cargo scenario must select an existing package with -p") from error


def cargo_inventory_errors(repo, inventory, *, list_cases=False):
    scenarios = [item for item in inventory["scenarios"] if is_cargo_test(item.get("command", []))]
    errors = []
    with tempfile.TemporaryDirectory(prefix="orbit-qa-cargo-inventory-") as tmp:
        env = isolated_environment(Path(tmp))
        metadata = run(["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"],
                       cwd=repo, env=env)
        try:
            if metadata.get("output_truncated"):
                raise ValueError("Cargo inventory metadata output was truncated")
            packages = {item["name"]: item["targets"]
                        for item in parse_json(metadata, "Cargo inventory metadata")["packages"]}
        except (ValueError, KeyError) as error:
            return [str(error)]
        for scenario in scenarios:
            try:
                validate_cargo_target(scenario["command"], packages)
                if list_cases:
                    command = list(scenario["command"])
                    if "--" not in command:
                        command.append("--")
                    command.append("--list")
                    evidence = run(command, cwd=repo, env=env, timeout=1800)
                    selected = validate_cargo_case_listing(evidence, scenario.get("required_tests", []))
                    print(json.dumps({"scenario": scenario["id"], "command": command,
                                      "selected_tests": selected, "behavioral_coverage": False}))
            except ValueError as error:
                errors.append(f"{scenario['id']}: {error}")
    return errors


def validate_cargo_case_listing(evidence, required_tests):
    """Listing verifies selection only; passing execution is still required."""
    if evidence["exit_code"] != 0 or evidence.get("output_truncated"):
        raise ValueError(f"Cargo case discovery failed: {evidence.get('stderr', '')}")
    selected = set(re.findall(r"^(\S+): test$", evidence.get("stdout", ""), re.MULTILINE))
    if not selected:
        raise ValueError("Cargo selection lists zero behavioral cases")
    missing = set(required_tests) - selected
    if missing:
        raise ValueError(f"Cargo selection omits required behavioral cases: {sorted(missing)}")
    return sorted(selected)


def cli_help_children(help_text):
    help_body = help_text.split("\nOptions:", 1)[0]
    usage = re.search(r"^Usage: [^\n]*(?:\n[ \t]+[^\n]*)*", help_body, re.MULTILINE)
    if not usage or not re.search(r"(?:<COMMAND>|\[COMMAND\])", usage.group(0)):
        return []
    children = []
    for line in help_body.splitlines():
        match = re.fullmatch(r"  ([a-z][a-z0-9-]*)(?:\s{2,}.*)?", line)
        if match and match.group(1) != "help":
            children.append(match.group(1))
    return children


def cli_command_paths(orbit_bin, top_level, cwd, env):
    """Probe executable help grammar; this is discovery, never behavioral coverage."""
    pending = [(name,) for name in sorted(top_level) if name != "plugin-group"]
    paths = []
    while pending:
        path = pending.pop(0)
        evidence = run([orbit_bin, *path, "--help"], cwd=cwd, env=env, timeout=30)
        if evidence["exit_code"] != 0:
            raise ValueError(f"help discovery failed for {' '.join(path)}: {evidence['stderr']}")
        paths.append(" ".join(path))
        # Task and run use grouped templates (Tasks:, Health:, Workflows:),
        # while clap's standard template uses Commands:. Stop before options
        # and examples, and only recurse when Usage declares a command slot.
        pending.extend((*path, child) for child in cli_help_children(evidence["stdout"]))
        if len(paths) + len(pending) > 1000:
            raise ValueError("CLI command discovery exceeded 1000 paths")
    return sorted(paths)


def candidate_source(repo: Path, excluded_paths=()):
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip()
    diff = subprocess.check_output(["git", "diff", "--binary", "HEAD"], cwd=repo)
    status = subprocess.check_output(["git", "status", "--porcelain=v1"], cwd=repo, text=True)
    excluded = tuple(str(path) for path in excluded_paths)

    def is_excluded(path):
        return any(path == excluded_path or path.startswith(f"{excluded_path}/")
                   for excluded_path in excluded)

    status_lines = [line for line in status.splitlines()
                    if not is_excluded(line[3:].split(" -> ")[-1] if len(line) > 3 else line)]
    untracked_output = subprocess.check_output(
        ["git", "ls-files", "--others", "--exclude-standard", "-z"], cwd=repo)
    untracked = []
    for raw_path in untracked_output.split(b"\0"):
        if not raw_path:
            continue
        path = raw_path.decode()
        candidate = repo / path
        if not is_excluded(path) and candidate.is_file():
            untracked.append((path, hashlib.sha256(candidate.read_bytes()).hexdigest()))
    material = json.dumps({"head": head, "diff": hashlib.sha256(diff).hexdigest(),
                           "untracked": untracked}, sort_keys=True).encode()
    return {"head": head, "diff_sha256": hashlib.sha256(diff).hexdigest(),
            "untracked": untracked, "candidate_id": hashlib.sha256(material).hexdigest(),
            "status": status_lines}


def finalize_result(evidence, assertions, candidate_id, failure=None, accept_nonzero=False):
    evidence["assertions"] = assertions
    evidence["candidate_id"] = candidate_id
    if (evidence["exit_code"] != 0 and not accept_nonzero) or failure:
        evidence["outcome"] = "FAIL"
        if failure:
            evidence["stderr"] = (evidence.get("stderr") or "") + "\nassertion: " + failure
    else:
        evidence["outcome"] = "PASS"
    return evidence


def parse_json(evidence, description):
    if evidence["exit_code"] != 0:
        raise ValueError(f"{description} exited {evidence['exit_code']}")
    if not evidence.get("stdout", "").strip():
        raise ValueError(f"{description} returned empty output")
    try:
        return json.loads(evidence["stdout"])
    except json.JSONDecodeError as error:
        raise ValueError(f"{description} returned invalid JSON: {error}") from error


def validate_npm_result(repo, command, evidence):
    if command != ["./scripts/smoke-npm-install.sh", "--local-package-check"]:
        raise ValueError("npm assertions require the local candidate package command")
    body = parse_json(evidence, "local npm package check")
    validate_npm_evidence(repo, body)
    evidence["npm_package_evidence"] = body
    archive = repo / body["archive"]["path"]
    evidence["retained_evidence"] = {
        "directory": str(archive.parent),
        "files": [{"path": archive.name, "sha256": body["archive"]["sha256"]}]}
    return body["assertions"]


def validate_cargo_test_evidence(command, evidence, required_tests=()):
    """Require completed, non-vacuous Rust tests, including command-kind suites."""
    if not is_cargo_test(command):
        return
    summaries = re.findall(
        r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;",
        evidence.get("stdout", "") + "\n" + evidence.get("stderr", ""))
    if not summaries:
        raise ValueError("cargo test returned no completed test-suite summary")
    passed = sum(int(row[1]) for row in summaries)
    failed = sum(int(row[2]) for row in summaries)
    evidence["test_counts"] = {"passed": passed, "failed": failed,
                               "ignored": sum(int(row[3]) for row in summaries),
                               "suites": len(summaries)}
    passing_tests = sorted(set(re.findall(
        r"^test (\S+) \.\.\. ok$",
        evidence.get("stdout", "") + "\n" + evidence.get("stderr", ""), re.MULTILINE)))
    evidence["passing_tests"] = passing_tests
    if failed or any(row[0] != "ok" for row in summaries):
        raise ValueError("cargo test reported failing tests")
    if passed == 0:
        raise ValueError("cargo test executed zero passing tests; check the selection")
    missing = set(required_tests) - set(passing_tests)
    if missing:
        raise ValueError(f"cargo test did not pass the required behavioral cases: {sorted(missing)}")


def scenario_decision(inventory, results, candidate_id):
    failures = []
    for scenario in inventory["scenarios"]:
        if not scenario["required"]:
            continue
        if scenario.get("kind") == "coverage-gap":
            failures.append(f"{scenario['id']}: required coverage gap: {scenario.get('coverage_gap')}")
            continue
        rows = [row for row in results if row["scenario"] == scenario["id"]]
        observed = {item for row in rows for item in row.get("assertions", [])}
        required = set(scenario["assertions"])
        if not rows or any(row["outcome"] != "PASS" for row in rows):
            failures.append(f"{scenario['id']}: missing, failed, or unrun evidence")
        elif observed != required:
            failures.append(f"{scenario['id']}: assertions missing={sorted(required-observed)} unexpected={sorted(observed-required)}")
        elif any(row.get("candidate_id") != candidate_id for row in rows):
            failures.append(f"{scenario['id']}: mixed or stale candidate evidence")
    for surface, reason in inventory.get("required_capability_gaps", {}).items():
        failures.append(f"{surface}: required capability gap: {reason}")
    return not failures, failures


def isolated_environment(temp: Path, host_env=None):
    keep = ["PATH", "USER", "LOGNAME", "LANG", "LC_ALL", "RUSTUP_TOOLCHAIN"]
    host_env = os.environ if host_env is None else host_env
    env = {key: host_env[key] for key in keep if key in host_env}
    user_home = Path.home()
    env.update({"HOME": str(temp / "home"), "USERPROFILE": str(temp / "home"),
                "XDG_CONFIG_HOME": str(temp / "xdg"), "TMPDIR": str(temp / "tmp"),
                "CARGO_HOME": host_env.get("CARGO_HOME", str(user_home / ".cargo")),
                "RUSTUP_HOME": host_env.get("RUSTUP_HOME", str(user_home / ".rustup"))})
    for path in (temp / "home", temp / "xdg", temp / "tmp"):
        path.mkdir(parents=True, exist_ok=True)
    return env


def browser_environment(base: dict, playwright_browsers_path: Path | None,
                        browser_ld_library_path: Path | None):
    """Add only declared browser capability paths to the disposable child env."""
    env = dict(base)
    if playwright_browsers_path:
        env["PLAYWRIGHT_BROWSERS_PATH"] = str(playwright_browsers_path)
    if browser_ld_library_path:
        env["LD_LIBRARY_PATH"] = str(browser_ld_library_path)
    return env


def inspect_browser_capability(playwright_module: Path, env: dict, repo: Path):
    probe = run([
        "node", "--input-type=module", "-e",
        "import { pathToFileURL } from 'node:url'; "
        "const { chromium } = await import(pathToFileURL(process.argv[1]).href); "
        "const browser = await chromium.launch({ headless: true }); "
        "console.log(browser.version()); await browser.close();",
        str(playwright_module),
    ], cwd=repo, env=env, timeout=180)
    version = probe["stdout"].strip() if probe["exit_code"] == 0 else None
    return probe, version


def add_result(results, scenario, evidence):
    evidence["scenario"] = scenario
    results.append(evidence)


def run_builtins(repo: Path, orbit_bin: str, temp: Path, env: dict, candidate_id: str):
    """Run the local built-in scenarios; return (results, halt_reason).

    A prerequisite step that fails stops the run with a reason, which the caller
    records on every scenario that was not reached.
    """
    results = []
    root = temp / "home/.orbit"
    work = temp / "workspace"
    other = temp / "other-workspace"
    for path in (work, other):
        path.mkdir()
        for argv in (["git", "init", "-q", "-b", "main"], ["git", "add", "README.md"]):
            if argv[1] == "add":
                (path / "README.md").write_text("fixture\n")
            evidence = run(argv, cwd=path, env=env)
            if evidence["exit_code"] != 0:
                raise RuntimeError(f"fixture setup failed: {evidence}")
        evidence = run(["git", "-c", "user.name=Orbit QA", "-c", "user.email=qa@example.invalid",
                        "commit", "-qm", "fixture"], cwd=path, env=env)
        if evidence["exit_code"] != 0:
            raise RuntimeError(f"fixture commit failed: {evidence}")

    def checked(scenario, argv, cwd, assertions, validator, *, accept_nonzero=False):
        evidence = run(argv, cwd=cwd, env=env)
        failure = None
        try:
            validator(evidence)
        except (AssertionError, KeyError, OSError, TypeError, ValueError) as error:
            failure = str(error)
        add_result(results, scenario, finalize_result(evidence, assertions, candidate_id, failure,
                                                       accept_nonzero=accept_nonzero))
        return evidence if failure is None else None

    def succeeds(evidence):
        if evidence["exit_code"] != 0:
            raise ValueError(f"command exited {evidence['exit_code']}")

    initialized = checked("isolated-cli-lifecycle",
            [orbit_bin, "init", "--non-interactive", "--skip-host-prerequisites",
             "--machine-name", "qa-machine", "--task-prefix", "QAF"],
            temp, ["global-init-persists-isolated-root"], succeeds)
    if initialized is None:
        return results, "disposable global init failed; later built-in scenarios depend on its isolated root"
    checked("isolated-cli-lifecycle", [orbit_bin, "workspace", "init", "--name", "qa-primary"],
            work, ["workspace-init-registers-primary"], succeeds)
    checked("workspace-boundary", [orbit_bin, "workspace", "init", "--name", "qa-other"],
            other, ["second-workspace-isolated"], succeeds)

    def primary_workspace(evidence):
        body = parse_json(evidence, "workspace show")
        observed = Path(body["checkout"]["repo_root"]).resolve()
        if observed != work.resolve():
            raise ValueError(f"wrong workspace: expected {work.resolve()}, got {observed}")

    checked("workspace-boundary",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "workspace", "show", "--format", "json"],
            other, ["explicit-selector-wins-over-cwd"], primary_workspace)

    destination = work / ".orbit/auto_tasks/qa-full-sweep.yaml"
    destination.parent.mkdir(parents=True, exist_ok=True)
    # Exercise auto-task registration without requiring the operator's ignored,
    # workspace-local sign-off definition to exist on the testing machine.
    destination.write_text("""schemaVersion: 1
name: qa-full-sweep
description: Disposable registration and manual mint fixture
enabled: false
schedule:
  cron: 0 0 * * *
template:
  title: Perform complete pre-release Orbit QA sign-off
  description: Verify disposable fixture registration and manual minting.
  acceptance_criteria: [Fixture task persists]
  task_type: chore
  complexity: low
  priority: low
  status: backlog
dedupe: skip_if_open
created_by: human
created_at: 2026-01-01T00:00:00Z
updated_by: human
updated_at: 2026-01-01T00:00:00Z
""")

    def task_added(evidence):
        body = parse_json(evidence, "task add")
        if body.get("title") != "QA behavioral fixture" or not body.get("id", "").startswith("QAF-"):
            raise ValueError("task add did not return the requested persisted task")

    task = checked("isolated-cli-lifecycle",
                   [orbit_bin, "--root", str(root), "--workspace", str(work), "task", "add",
                    "--title", "QA behavioral fixture", "--description", "unique qa search marker",
                    "--complexity", "low", "--status", "backlog",
                    "--acceptance-criteria", "fixture round trip", "--json"], work,
                   ["task-add-returns-requested-fields"], task_added)
    if task is None:
        return results, "disposable task add failed; later built-in scenarios depend on the fixture task"
    task_id = json.loads(task["stdout"])["id"]
    payload = work / "qa-artifact.txt"
    payload.write_text("qa evidence payload\n")

    def shown(evidence):
        body = parse_json(evidence, "task show")
        if body.get("id") != task_id or body.get("title") != "QA behavioral fixture":
            raise ValueError("task show did not read back the created task")
        if body.get("acceptance_criteria") != ["fixture round trip"]:
            raise ValueError("task acceptance criteria changed during round trip")

    checked("isolated-cli-lifecycle",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "task", "show", task_id, "--json"],
            work, ["task-show-round-trips-payload"], shown)
    checked("isolated-cli-lifecycle",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "task", "artifact", "put",
             task_id, str(payload), "--path", "qa/evidence.txt", "--model", "codex", "--json"],
            work, ["artifact-put-mutates-task"], lambda evidence: parse_json(evidence, "artifact put"))

    def artifact_read(evidence):
        if evidence["exit_code"] != 0 or evidence["stdout"] != "qa evidence payload\n":
            raise ValueError("artifact get did not return exact persisted bytes")

    checked("isolated-cli-lifecycle",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "task", "artifact", "get",
             task_id, "qa/evidence.txt"], work, ["artifact-get-round-trips-exact-content"], artifact_read)

    def listed(evidence):
        body = parse_json(evidence, "task list")
        rows = body if isinstance(body, list) else body.get("items", body.get("tasks", []))
        if task_id not in {row.get("id") for row in rows}:
            raise ValueError("task list omitted the created task")

    checked("isolated-cli-lifecycle",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "task", "list", "--limit", "20", "--json"],
            work, ["task-list-contains-created-task"], listed)

    def refused(evidence):
        if evidence["exit_code"] == 0:
            raise ValueError("cross-workspace task show was not refused")

    checked("workspace-boundary",
            [orbit_bin, "--root", str(root), "--workspace", str(other), "task", "show", task_id, "--json"],
            other, ["cross-workspace-task-read-refused"], refused, accept_nonzero=True)

    config_path = work / ".orbit/config.toml"
    config_set = checked("config-transaction",
                         [orbit_bin, "--root", str(root), "--workspace", str(work), "config", "set",
                          "workflow.base_branch", "qa-candidate", "--fresh"], work,
                         ["supported-setting-persists"], succeeds)
    if config_set is None:
        return results, "disposable config set failed; later built-in scenarios depend on the persisted setting"

    def config_readback(evidence):
        body = parse_json(evidence, "config get")
        if body.get("key") != "workflow.base_branch" or body.get("value") != "qa-candidate":
            raise ValueError("config get did not return the exact persisted value")
        if body.get("scope") != "workspace" or Path(body.get("path", "")).resolve() != config_path.resolve():
            raise ValueError("config get read from the wrong scope or path")

    checked("config-transaction",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "config", "get",
             "workflow.base_branch", "--scope", "workspace", "--format", "json"], work,
            ["exact-setting-readback"], config_readback)
    config_before_invalid = config_path.read_bytes()

    def invalid_config_refused(evidence):
        refused(evidence)
        if config_path.read_bytes() != config_before_invalid:
            raise ValueError("invalid config write changed config.toml bytes")

    checked("config-transaction",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "config", "set",
             "execution.codex.sandbox", "not-a-real-mode"], work,
            ["invalid-setting-refused-without-byte-change"], invalid_config_refused,
            accept_nonzero=True)

    identity_path = root / "config.toml"
    registry_path = root / "workspaces.json"

    def machine_identity(evidence):
        body = parse_json(evidence, "config show")
        settings = body.get("settings", {})
        if settings.get("machine.name") != "qa-machine" or settings.get("machine.task_prefix") != "QAF":
            raise ValueError("config show did not return the initialized identity")
        machine_id = settings.get("machine.id")
        if not isinstance(machine_id, str) or not machine_id.startswith("hm_"):
            raise ValueError("config show returned an invalid stable machine.id")

    initial_machine = checked("machine-identity-transaction",
                              [orbit_bin, "--root", str(root), "--workspace", str(work),
                               "config", "show", "--scope", "global", "--format", "json"],
                              work, ["initialized-identity-readback"], machine_identity)
    initial_machine_id = (
        json.loads(initial_machine["stdout"])["settings"]["machine.id"] if initial_machine else None
    )
    checked("machine-identity-transaction",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "config", "set",
             "--global", "machine.name", "qa-renamed"], work,
            ["supported-rename-updates-the-machine-name"], succeeds)

    def renamed_identity(evidence):
        body = parse_json(evidence, "renamed config show")
        settings = body.get("settings", {})
        if settings.get("machine.id") != initial_machine_id or settings.get("machine.name") != "qa-renamed":
            raise ValueError("rename changed stable identity or failed machine.name readback")

    checked("machine-identity-transaction",
            [orbit_bin, "--root", str(root), "--workspace", str(work),
             "config", "show", "--scope", "global", "--format", "json"], work,
            ["rename-preserves-machine-id-and-reads-back"], renamed_identity)
    identity_before_invalid = identity_path.read_bytes()
    registry_before_invalid = registry_path.read_bytes()

    def invalid_identity_refused(evidence):
        refused(evidence)
        if (identity_path.read_bytes() != identity_before_invalid
                or registry_path.read_bytes() != registry_before_invalid):
            raise ValueError("refused machine edit changed identity or registry bytes")

    # `machine.id` and `machine.task_prefix` are read-only, a workspace layer
    # may not carry `[machine]` at all, and a malformed name is refused — each
    # without touching a byte of either file.
    checked("machine-identity-transaction",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "config", "set",
             "--global", "machine.id", "hm_forged"], work,
            ["immutable-machine-id-refused-without-mutation"],
            invalid_identity_refused, accept_nonzero=True)
    checked("machine-identity-transaction",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "config", "set",
             "--global", "machine.task_prefix", "ZZ"], work,
            ["immutable-task-prefix-refused-without-mutation"],
            invalid_identity_refused, accept_nonzero=True)
    checked("machine-identity-transaction",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "config", "set",
             "machine.name", "workspace-scoped"], work,
            ["workspace-scoped-machine-key-refused-without-mutation"],
            invalid_identity_refused, accept_nonzero=True)
    checked("machine-identity-transaction",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "config", "set",
             "--global", "machine.name", "invalid/name"], work,
            ["invalid-rename-refused-without-mutation"],
            invalid_identity_refused, accept_nonzero=True)

    log_path = temp / "qa-unified-log.jsonl"
    selected_event = {
        "timestamp": "2026-09-10T01:02:03.000000000Z", "level": "INFO",
        "target": "orbit.qa.selected", "fields": {"message": "qa exact record", "case": 12062},
    }
    ignored_event = {
        "timestamp": "2026-09-10T01:02:04.000000000Z", "level": "DEBUG",
        "target": "orbit.qa.ignored", "fields": {"message": "must not be selected"},
    }
    log_path.write_text("\n".join([json.dumps(ignored_event), "not-json", json.dumps(selected_event)]) + "\n")

    def exact_log_record(evidence):
        succeeds(evidence)
        lines = [json.loads(line) for line in evidence["stdout"].splitlines() if line.strip()]
        if lines != [selected_event]:
            raise ValueError(f"log tail selected the wrong records: {lines!r}")

    checked("unified-log-selection",
            [orbit_bin, "--root", str(root), "log", "tail", "-n", "10", "--target",
             "orbit.qa.selected", "--level", "info", "--path", str(log_path), "--format", "ndjson"],
            work, ["exact-filtered-unified-record"], exact_log_record)

    def empty_log_selection(evidence):
        succeeds(evidence)
        if evidence["stdout"] != "":
            raise ValueError("negative log filter returned a record")

    checked("unified-log-selection",
            [orbit_bin, "--root", str(root), "log", "tail", "-n", "10", "--target",
             "orbit.qa.absent", "--path", str(log_path), "--format", "ndjson"], work,
            ["nonmatching-and-malformed-records-are-excluded"], empty_log_selection)

    fixture_job = temp / "qa-legacy-logs.yaml"
    fixture_job.write_text("""schemaVersion: 2
kind: Job
metadata:
  name: qa_legacy_logs_fixture
spec:
  state: enabled
  kind: workflow
  max_active_runs: 1
  steps:
    - id: exact_step
      default_input:
        seconds: 0
      spec:
        type: deterministic
        action: sleep
        config: {}
""")

    def completed_fixture_run(evidence):
        body = parse_json(evidence, "fixture job run")
        # Canonical vocabulary is JobRunState::Success.to_string() == "success"
        # (crates/orbit-types/src/workflow/job.rs), not the legacy "succeeded" literal.
        if body.get("state") != "success" or not body.get("run_id"):
            raise ValueError("deterministic fixture job did not succeed")

    fixture_run = checked("legacy-logs-compatibility",
                          [orbit_bin, "--root", str(root), "--workspace", str(work), "run", "job",
                           str(fixture_job), "--input", "crew=sol", "--wait", "--format", "json"],
                          work, ["supported-run-fixture-completes"], completed_fixture_run)
    fixture_run_id = json.loads(fixture_run["stdout"])["run_id"] if fixture_run else "missing-run"

    def legacy_logs_output(evidence):
        body = parse_json(evidence, "legacy logs")
        # The fixture's single deterministic step is recorded on the run, so the
        # legacy view renders exactly one legacy_step_to_json entry for it; only
        # the timing fields are left unchecked.
        expected = {"step_index": 0, "target_id": "exact_step", "target_type": "activity",
                    "state": "success", "exit_code": None, "error_code": None,
                    "error_message": None}
        observed = {key: body[0].get(key) for key in expected} if isinstance(body, list) and len(body) == 1 else None
        if observed != expected:
            raise ValueError(f"legacy logs changed its exact detached-run compatibility output: {body!r}")
        if "[deprecated]" not in evidence.get("stderr", ""):
            raise ValueError("legacy logs omitted its compatibility deprecation notice")

    checked("legacy-logs-compatibility",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "logs", fixture_run_id,
             "--format", "json"], work,
            ["exact-legacy-compatibility-output-and-deprecation"], legacy_logs_output)
    checked("legacy-logs-compatibility",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "logs", fixture_run_id,
             "--step", "absent-step", "--format", "json"], work,
            ["unknown-legacy-step-is-refused"], refused, accept_nonzero=True)

    migration_root = temp / "migration-root"
    migration_init = checked(
        "migration-lifecycle",
        [orbit_bin, "--root", str(migration_root), "init", "--non-interactive",
         "--skip-host-prerequisites", "--machine-name", "qa-migration", "--task-prefix", "QAM"],
        temp, ["disposable-migration-fixture-initialized"], succeeds,
    )
    if migration_init is None:
        return results, "disposable migration-root init failed; later built-in scenarios depend on it"
    migration_marker = migration_root / "state/layout.version"
    migration_marker.parent.mkdir(parents=True, exist_ok=True)
    migration_marker.write_text("1\n")
    migration_before = migration_marker.read_bytes()

    def pending_migration(evidence):
        refused(evidence)
        if "migration(s) pending" not in evidence.get("stderr", ""):
            raise ValueError("dry-run did not report pending migrations")
        if migration_marker.read_bytes() != migration_before:
            raise ValueError("migration dry-run changed the legacy marker")

    checked("migration-lifecycle",
            [orbit_bin, "--root", str(migration_root), "migrate", "--dry-run", "--json"],
            temp, ["legacy-dry-run-reports-pending-without-mutation"], pending_migration,
            accept_nonzero=True)

    def applied_migration(evidence):
        body = parse_json(evidence, "confirmed migration")
        layout = body.get("layout", {})
        if body.get("up_to_date") is not True or layout.get("current") != layout.get("supported"):
            raise ValueError("confirmed migration did not reach the supported layout")
        applied = body.get("applied_layout", [])
        if not applied or applied[0].get("version") != 2:
            raise ValueError("confirmed migration did not report the known legacy transition")

    checked("migration-lifecycle",
            [orbit_bin, "--root", str(migration_root), "migrate", "--confirm", "--json"],
            temp, ["confirmed-migration-applies-known-legacy-state"], applied_migration)

    def idempotent_migration(evidence):
        body = parse_json(evidence, "repeated migration")
        if body.get("up_to_date") is not True or body.get("applied_layout") != []:
            raise ValueError("repeated migration was not idempotent")

    checked("migration-lifecycle",
            [orbit_bin, "--root", str(migration_root), "migrate", "--confirm", "--json"],
            temp, ["repeated-migration-is-idempotent"], idempotent_migration)
    migration_marker.write_text("99\n")
    newer_before = migration_marker.read_bytes()
    migration_identity_path = migration_root / "config.toml"
    migration_identity_before = migration_identity_path.read_bytes()

    def newer_migration_refused(evidence):
        refused(evidence)
        if "newer" not in evidence.get("stderr", ""):
            raise ValueError("newer migration refusal did not explain the incompatibility")
        if (migration_marker.read_bytes() != newer_before
                or migration_identity_path.read_bytes() != migration_identity_before):
            raise ValueError("newer migration refusal changed marker or machine identity bytes")

    checked("migration-lifecycle",
            [orbit_bin, "--root", str(migration_root), "migrate", "--dry-run", "--json"],
            temp, ["newer-state-refused-without-mutation"], newer_migration_refused,
            accept_nonzero=True)

    def auto_task_show(evidence):
        body = parse_json(evidence, "auto-task show")
        if body.get("name") != "qa-full-sweep" or body.get("enabled") is not False:
            raise ValueError("manual full sweep registration is missing or enabled")

    checked("auto-task-registration-mint",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "auto-task", "show",
             "qa-full-sweep", "--format", "json"], work,
            ["definition-is-present-and-disabled"], auto_task_show)

    def minted(evidence):
        body = parse_json(evidence, "auto-task mint")
        if not body.get("id") or body.get("title") != "[auto-task] Perform complete pre-release Orbit QA sign-off":
            raise ValueError("manual mint did not create the declared task")

    checked("auto-task-registration-mint",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "auto-task", "mint",
             "qa-full-sweep", "--json"], work, ["manual-mint-persists-declared-task"], minted)

    def catalog(evidence):
        body = parse_json(evidence, "job list")
        rows = body if isinstance(body, list) else body.get("items", [])
        names = {row.get("name") or row.get("id") or row.get("job_id") for row in rows}
        expected = {"task_pilot_pipeline", "worktree_gc_pipeline"}
        if not expected.issubset(names):
            raise ValueError(f"job catalog missing {sorted(expected-names)}")
    checked("workflow-definition-boundary",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "job", "list", "--format", "json"],
            work, ["job-catalog-returns-structured-installed-definitions"], catalog)

    def step_activities(evidence):
        body = parse_json(evidence, "job show")
        if "activity:prepare_task_pilot" not in json.dumps(body.get("steps")):
            raise ValueError("job show does not name the activity its steps run")
    checked("workflow-definition-boundary",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "job", "show",
             "task_pilot_pipeline", "--format", "json"],
            work, ["job-show-names-step-activities"], step_activities)

    def search_result(evidence):
        body = parse_json(evidence, "search")
        encoded = json.dumps(body)
        if task_id not in encoded or "QA behavioral fixture" not in encoded:
            raise ValueError("lexical search did not return the created task")

    checked("search-observability-boundary",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "search",
             "unique qa search marker", "--kind", "task", "--json"], work,
            ["search-returns-created-task"], search_result)

    def friction_added(evidence):
        body = parse_json(evidence, "friction add")
        if "qa isolated friction marker" not in json.dumps(body):
            raise ValueError("friction add did not return persisted marker")

    checked("search-observability-boundary",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "friction", "add",
             "--title", "QA friction", "--body", "qa isolated friction marker", "--model", "codex", "--json"],
            work, ["friction-add-persists-record"], friction_added)
    checked("search-observability-boundary",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "tool", "run", "orbit.task.show",
             "--input", json.dumps({"id":task_id,"model":"codex"}), "--format", "json"],
            work, [], lambda evidence: parse_json(evidence, "tool task.show"))
    checked("search-observability-boundary",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "search", "reindex", "--json"],
            work, ["search-reindex-reports-chunks"], lambda evidence: parse_json(evidence, "search reindex"))
    checked("search-observability-boundary",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "doctor", "--json"],
            work, ["doctor-reports-isolated-workspace-health"], lambda evidence: parse_json(evidence, "doctor"))

    for command, assertion in ((["tool", "list", "--format", "json"], "tool-definitions-are-structured"),
                               (["plugin", "list", "--format", "json"], "plugin-inventory-is-structured"),
                               (["skill", "list", "--format", "json"], "skill-definitions-are-structured")):
        checked("definition-policy-boundary",
                [orbit_bin, "--root", str(root), "--workspace", str(work), *command], work,
                [assertion], lambda evidence, assertion=assertion: parse_json(evidence, assertion))

    plugin_source = temp / "qa-plugin"
    # A plugin source keeps its plugin in `.orbit-plugin/`, the only tree installed.
    plugin_root = plugin_source / ".orbit-plugin"
    (plugin_root / "bin").mkdir(parents=True)
    backend = plugin_root / "bin/backend.sh"
    backend.write_text("#!/bin/sh\ninput=$(cat)\nprintf '{\"ok\":true,\"output\":{\"echo\":%s}}\\n' \"$input\"\n")
    backend.chmod(0o755)
    (plugin_root / "plugin.yaml").write_text("""schemaVersion: 2
kind: Plugin
metadata:
  name: qashapes
  version: 0.1.0
  description: Isolated QA command group.
spec:
  backend:
    type: exec
    command: bin/backend.sh
  tools:
    - name: recommend
      description: Echo schema-derived input.
      execution_kind: read_only
      mcp_scope: workspace
      input_schema:
        type: object
        properties:
          query: { type: string }
          max_depth: { type: integer }
          tags: { type: array, items: { type: string } }
      cli:
        positional: [query]
""")
    plugin_base = [orbit_bin, "--root", str(root), "--workspace", str(work)]
    added = checked("definition-policy-boundary",
                    [*plugin_base, "plugin", "add", str(plugin_source)], work, [], succeeds)
    enabled = (checked("definition-policy-boundary",
                       [*plugin_base, "plugin", "enable", "qashapes"], work, [], succeeds)
               if added else None)
    if enabled:
        plugin_input = {"query": "leakage", "max_depth": 3, "tags": ["rust", "cli"]}
        operator_env = {**env, "ORBIT_OPERATOR": "1"}
        group = run([*plugin_base, "qashapes", "recommend", "leakage", "--max-depth", "3",
                     "--tags", "rust", "--tags", "cli", "--format", "json"],
                    cwd=work, env=operator_env)
        tool = run([*plugin_base, "tool", "run", "qashapes.recommend", "--input",
                    json.dumps(plugin_input), "--format", "json"], cwd=work, env=operator_env)
        failure = None
        try:
            group_body = parse_json(group, "plugin command group")
            tool_body = parse_json(tool, "plugin tool run")
            if group_body != tool_body or group_body.get("echo", {}).get("input") != plugin_input:
                raise ValueError("plugin command group and tool run returned different behavior")
        except (AttributeError, TypeError, ValueError) as error:
            failure = str(error)
        add_result(results, "definition-policy-boundary",
                   finalize_result(tool, [], candidate_id))
        add_result(results, "definition-policy-boundary",
                   finalize_result(group,
                                   ["plugin-derived-command-groups-match-tool-run"] if failure is None else [],
                                   candidate_id, failure))

    def fs_access(evidence):
        body = parse_json(evidence, "doctor fs-access")
        if body.get("read", {}).get("allowed") is not True or body.get("modify", {}).get("allowed") is not False:
            raise ValueError("implementer must read but not modify the workspace task store")
    checked("definition-policy-boundary",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "doctor", "fs-access",
             "implementer", ".orbit/tasks/x", "--json"], work,
            ["doctor-fs-access-dry-runs-profile"], fs_access)

    def providers(evidence):
        rows = parse_json(evidence, "doctor providers")
        if not rows or any("sandbox" not in row or "cli_available" not in row for row in rows):
            raise ValueError("doctor providers must report each executor's CLI and sandbox")
    checked("definition-policy-boundary",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "doctor", "providers",
             "--format", "json"], work,
            ["doctor-providers-report-executor-sandbox"], providers)

    initialize = {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"qa","version":"1"}}}
    requests = [initialize,
        {"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}},
        {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"orbit_workspace_list","arguments":{}}},
        {"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"orbit_task_show","arguments":{"id":task_id,"workspace":str(work),"model":"codex"}}},
        {"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"orbit_task_show","arguments":{"id":task_id,"workspace":str(other),"model":"codex"}}}]
    mcp_evidence = run([orbit_bin, "mcp", "serve", "--workspace", str(work)], cwd=work,
                       env=env, input_text="".join(json.dumps(item)+"\n" for item in requests),
                       timeout=30)
    failure = None
    try:
        responses = {body["id"]: body for line in mcp_evidence["stdout"].splitlines()
                     if line.strip() and (body := json.loads(line)).get("id") is not None}
        tool_names = {tool["name"] for tool in responses[2]["result"]["tools"]}
        if "orbit_task_show" not in tool_names:
            raise ValueError("tools/list omitted orbit_task_show")
        if task_id not in json.dumps(responses[4]) or "error" in responses[4]:
            raise ValueError("MCP task.show did not return the bound task")
        if "error" not in responses[5] and not responses[5].get("result", {}).get("isError"):
            raise ValueError("MCP cross-workspace task.show was not refused")
        workspace_names = {item.get("name") for item in responses[3]["result"]["structuredContent"]["workspaces"]}
        if "qa-primary" not in workspace_names:
            raise ValueError("MCP workspace.list omitted the bound workspace")
    except (KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
        failure = str(error)
    add_result(results, "mcp-wire-boundary", finalize_result(mcp_evidence,
        ["initialize-negotiates-jsonrpc", "tools-list-is-structured", "workspace-list-returns-bound-checkout",
         "task-show-round-trips-over-mcp", "cross-workspace-mcp-read-refused"], candidate_id, failure))

    web_assertions = ["healthz-is-ready", "workspace-api-identifies-bound-checkout",
                      "task-api-reads-persisted-task"]
    try:
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
    except OSError as error:
        evidence = {"command": ["bind", "127.0.0.1"], "exit_code": None,
                    "stdout": "", "stderr": str(error)}
        add_result(results, "dashboard-api-boundary",
                   finalize_result(evidence, web_assertions, candidate_id,
                                   "loopback listener unavailable"))
        return results, None
    web = subprocess.Popen([orbit_bin, "--root", str(root), "web", "serve", "--host", "127.0.0.1", "--port", str(port), "--no-open"],
                           cwd=work, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    web_evidence = {"command":["GET","/healthz","GET","/api/workspaces","GET","/api/tasks"],
                    "exit_code":1,"stdout":"","stderr":"dashboard did not become ready","outcome":"FAIL"}
    failure = "dashboard did not become ready"
    try:
        for _ in range(80):
            if web.poll() is not None:
                break
            try:
                health = urllib.request.urlopen(f"http://127.0.0.1:{port}/healthz", timeout=1).read().decode()
                workspaces = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{port}/api/workspaces", timeout=1).read())
                tasks = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{port}/api/tasks", timeout=1).read())
                combined = json.dumps({"health":health,"workspaces":workspaces,"tasks":tasks})
                if str(work.resolve()) not in combined or task_id not in combined:
                    raise ValueError("API responses do not identify the fixture workspace and task")
                web_evidence.update({"exit_code":0,"stdout":combined,"stderr":""})
                failure = None
                break
            except (OSError, json.JSONDecodeError, ValueError):
                time.sleep(.1)
    finally:
        web.terminate()
        try:
            web.wait(timeout=5)
        except subprocess.TimeoutExpired:
            web.kill()
        captured_stdout, captured_stderr = web.communicate()
        if failure:
            web_evidence["stdout"] = captured_stdout
            web_evidence["stderr"] = captured_stderr or web_evidence["stderr"]
    add_result(results, "dashboard-api-boundary", finalize_result(web_evidence,
        web_assertions,
        candidate_id, failure))
    return results, None


def validate_hosted_macos_evidence(body, scenario, candidate, repo):
    if body.get("evidence_type") != "orbit-macos-platform" or body.get("platform") != "macos":
        raise ValueError("hosted evidence has the wrong type or platform")
    if candidate.get("status"):
        raise ValueError("hosted evidence requires a clean exact-checkout candidate")
    if body.get("source_revision") != candidate.get("head"):
        raise ValueError("hosted evidence is stale or for a different checkout")
    if body.get("command") != scenario.get("command"):
        raise ValueError("hosted evidence command does not match the required check")
    if body.get("outcome") != "PASS":
        raise ValueError("hosted evidence check failed or was not run")
    if body.get("assertions") != scenario.get("assertions"):
        raise ValueError("hosted evidence assertions are missing or unexpected")

    producer = body.get("producer", {})
    required_producer = ["repository", "run_id", "run_attempt", "workflow_ref", "workflow_sha"]
    if producer.get("system") != "github-actions" or any(not producer.get(key) for key in required_producer):
        raise ValueError("hosted evidence lacks authenticated GitHub Actions provenance")
    if not str(producer["run_id"]).isdigit() or not str(producer["run_attempt"]).isdigit():
        raise ValueError("hosted evidence has invalid GitHub Actions run identity")
    if ".github/workflows/ci-macos.yml@" not in str(producer["workflow_ref"]):
        raise ValueError("hosted evidence came from the wrong workflow")
    if not re.fullmatch(r"[0-9a-f]{40}", str(producer["workflow_sha"])):
        raise ValueError("hosted evidence has an invalid workflow revision")

    expected_identity = {
        path: hashlib.sha256((repo / path).read_bytes()).hexdigest()
        for path in [
            "scripts/check-ci-macos.sh", "scripts/qa-full-sweep-inventory.json",
            "scripts/test-qa-full-sweep.py", ".github/workflows/ci-macos.yml",
        ]
    }
    if body.get("source_identity") != expected_identity:
        raise ValueError("hosted evidence source identity does not match the checkout")


def process_self_test():
    from unittest import mock

    with tempfile.TemporaryDirectory(prefix="orbit-qa-process-self-test-") as directory:
        temp = Path(directory)
        env = isolated_environment(temp)
        success = run([sys.executable, "-c", "import sys; print('done'); sys.stderr.write('diagnostic')"],
                      cwd=temp, env=env)
        if success["exit_code"] != 0 or success["stdout"] != "done\n" or success["stderr"] != "diagnostic":
            raise AssertionError(f"supervised success lost output/status: {success}")
        failed = run([sys.executable, "-c", "raise SystemExit(7)"], cwd=temp, env=env)
        if failed["exit_code"] != 7 or failed["outcome"] != "FAIL":
            raise AssertionError("supervised failure was reported as success")
        lost = run([sys.executable, "-c",
                    "import os,signal,time; os.kill(os.getppid(),signal.SIGKILL); time.sleep(10)"],
                   cwd=temp, env=env, timeout=3)
        if lost["outcome"] != "FAIL" or lost["exit_code"] is not None:
            raise AssertionError("lost supervisor was reported as success")
        missing = run([str(temp / "missing-command")], cwd=temp, env=env)
        if missing["outcome"] != "FAIL" or not missing["stderr"]:
            raise AssertionError("failed command start was reported as success")
        stdin = run([sys.executable, "-c", "import sys; print(sys.stdin.read())"],
                    cwd=temp, env=env, input_text="wire input")
        if stdin["stdout"] != "wire input\n":
            raise AssertionError("supervisor did not preserve command stdin")
        blocked_input = run([sys.executable, "-c", "import time; time.sleep(10)"],
                            cwd=temp, env=env, input_text="x" * 2_000_000, timeout=.2)
        if blocked_input["exit_code"] is not None or blocked_input["outcome"] != "FAIL":
            raise AssertionError("blocked stdin escaped the command timeout")
        large = run([sys.executable, "-c",
                     "import sys; sys.stdout.write('界'*700000+'終'); sys.stderr.write('e'*2000000)"],
                    cwd=temp, env=env)
        if (large["exit_code"] != 0 or not large["output_truncated"]
                or not large["stdout"].endswith("終") or len(large["stderr"]) != _OUTPUT_LIMIT
                or len(large["stdout"]) > _OUTPUT_LIMIT):
            raise AssertionError("streamed output was not bounded or lost its Unicode tail")
        marker = temp / "grandchild-survived"
        child = "import time; from pathlib import Path; time.sleep(2); Path(" + repr(str(marker)) + ").write_text('survived')"
        parent = "import subprocess,sys,time; subprocess.Popen([sys.executable,'-c'," + repr(child) + "]); print('spawned',flush=True); time.sleep(20)"
        timed_out = run([sys.executable, "-c", parent], cwd=temp, env=env, timeout=1)
        if timed_out["exit_code"] is not None or timed_out["outcome"] != "FAIL" or "spawned" not in timed_out["stdout"]:
            raise AssertionError("timeout cleanup fixture did not start or was reported as success")
        # A clean parent exit must sweep inherited-pipe descendants too.
        completed_parent = parent.replace("time.sleep(20)", "raise SystemExit(0)")
        clean = run([sys.executable, "-c", completed_parent], cwd=temp, env=env, timeout=3)
        if clean["exit_code"] != 0:
            raise AssertionError("clean parent with pipe-inheriting descendant did not complete")
        with mock.patch("os.killpg", side_effect=PermissionError("fixture signal refusal")):
            refused = run([sys.executable, "-c", parent], cwd=temp, env=env, timeout=.5)
        if refused["outcome"] != "FAIL" or refused["cleanup_verified"]:
            raise AssertionError("refused group cleanup was reported as verified or successful")
        original_wait = subprocess.Popen.wait
        waits = []

        def expired_first_reap(process, timeout=None):
            waits.append(timeout)
            if len(waits) == 1:
                raise subprocess.TimeoutExpired(process.args, timeout)
            return original_wait(process, timeout=timeout)

        with (mock.patch.object(subprocess.Popen, "wait", expired_first_reap),
              mock.patch.object(subprocess.Popen, "kill", side_effect=PermissionError("fixture direct kill refusal"))):
            expired = run([sys.executable, "-c", "print('complete')"], cwd=temp, env=env)
        if (expired["outcome"] != "FAIL" or expired["cleanup_verified"]
                or any(value is None for value in waits) or "supervisor direct kill failed" not in expired["stderr"]):
            raise AssertionError("reap deadline did not retain a bounded, unverified failure")
        time.sleep(2.1)
        if marker.exists():
            raise AssertionError("supervised command left a grandchild after timeout or clean exit")

        started = temp / "owner-started"
        crashed_marker = temp / "owner-crash-grandchild"
        crash_child = "from pathlib import Path; import time; Path(" + repr(str(started)) + ").write_text('started'); time.sleep(2); Path(" + repr(str(crashed_marker)) + ").write_text('survived')"
        crash_parent = "import subprocess,sys,time; subprocess.Popen([sys.executable,'-c'," + repr(crash_child) + "]); time.sleep(20)"
        launcher = ("import importlib.util; from pathlib import Path; "
                    "s=importlib.util.spec_from_file_location('qa'," + repr(str(Path(__file__).resolve())) + "); "
                    "q=importlib.util.module_from_spec(s); s.loader.exec_module(q); "
                    "q.run([" + repr(sys.executable) + ",'-c'," + repr(crash_parent) + "],cwd=Path(" + repr(str(temp)) + "),env=q.isolated_environment(Path(" + repr(str(temp)) + ")),timeout=20)")
        owner = subprocess.Popen([sys.executable, "-c", launcher], env=env,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            deadline = time.monotonic() + 5
            while not started.exists() and owner.poll() is None and time.monotonic() < deadline:
                time.sleep(.02)
            if not started.exists():
                raise AssertionError("owner-crash fixture did not start")
            owner.kill()
            owner.wait(timeout=5)
            time.sleep(2.1)
            if crashed_marker.exists():
                raise AssertionError("QA owner's death left a command running")
        finally:
            if owner.poll() is None:
                owner.kill()
                owner.wait(timeout=5)


def builtin_init_self_test():
    """Disposable init must skip host preparation; an init failure must halt with a reason."""
    with tempfile.TemporaryDirectory(prefix="orbit-qa-builtin-self-test-") as directory:
        temp = Path(directory)
        repo = temp / "repo"
        repo.mkdir()
        env = isolated_environment(temp)
        log = temp / "argv.log"
        stub = temp / "orbit"
        stub.write_text(
            "#!/usr/bin/env python3\n"
            "import sys\n"
            f"open({str(log)!r}, 'a').write(' '.join(sys.argv[1:]) + '\\n')\n"
            "sys.stderr.write('Linux sandbox preparation does not support this distribution')\n"
            "raise SystemExit(1)\n")
        stub.chmod(0o755)
        results, halt = run_builtins(repo, str(stub), temp, env, "self-test-candidate")
        invocations = log.read_text().splitlines()
        init_lines = [line for line in invocations if " init " in f" {line} "]
        if not init_lines or any("--skip-host-prerequisites" not in line for line in init_lines):
            raise AssertionError(f"disposable init did not skip host preparation: {init_lines}")
        init_results = [row for row in results if row["scenario"] == "isolated-cli-lifecycle"]
        if len(init_results) != 1 or init_results[0]["outcome"] != "FAIL":
            raise AssertionError("failed disposable init was not reported as a single explicit failure")
        if len(results) != 1 or not halt or "global init failed" not in halt:
            raise AssertionError("failed disposable init did not halt with an explanatory reason")
        if any(" workspace " in f" {line} " for line in invocations):
            raise AssertionError("dependent scenarios ran after a failed disposable init")


def npm_pack_shape_self_test(candidate, body):
    """Both `npm pack --json` shapes (npm 11 array, npm 12 name-keyed object) are accepted;
    anything but exactly one correctly named package is refused."""
    row = json.loads(body["pack"]["stdout"])
    row = row[0] if isinstance(row, list) else next(iter(row.values()))
    other = {**row, "name": "@invalid/cli"}
    shapes = {"array": ([row], True), "object": ({row["name"]: row}, True),
              "two-packages": ([row, other], False), "empty": ([], False),
              "object-key-mismatch": ({"@invalid/cli": row}, False),
              "object-two-packages": ({row["name"]: row, other["name"]: other}, False)}
    for shape, (stdout, accepted) in shapes.items():
        variant = json.loads(json.dumps(body))
        variant["pack"]["stdout"] = json.dumps(stdout)
        try:
            validate_npm_evidence(candidate, variant)
        except ValueError:
            if accepted:
                raise AssertionError(f"npm pack {shape} output was refused")
        else:
            if not accepted:
                raise AssertionError(f"npm pack {shape} output earned npm evidence")


def npm_package_self_test():
    """Exercise the inventory command on disposable candidate inputs, never npm publication."""
    repo = Path(__file__).resolve().parent.parent
    scratch = repo / ".orbit/tmp"
    scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="npm-controls-", dir=scratch) as directory:
        temp = Path(directory)
        command = ["./scripts/smoke-npm-install.sh", "--local-package-check"]
        cases = ["valid", "malformed-npm", "malformed-server", "malformed-cargo",
                 "npm-drift", "server-drift", "registry-drift", "cargo-drift",
                 "missing-runtime", "excluded-runtime", "identity-drift", "lifecycle-scripts"]
        for case in cases:
            candidate = temp / case
            shutil.copytree(repo / "npm", candidate / "npm")
            (candidate / "scripts").mkdir()
            for script in ("smoke-npm-install.sh", "check_npm_package.py", "require-python.sh"):
                shutil.copy2(repo / "scripts" / script, candidate / "scripts" / script)
            for path in ("Cargo.toml", "server.json"):
                shutil.copyfile(repo / path, candidate / path)
            npm_path = candidate / "npm/package.json"
            server_path = candidate / "server.json"
            if case.startswith("malformed-"):
                target = {"npm": npm_path, "server": server_path, "cargo": candidate / "Cargo.toml"}[case.split("-", 1)[1]]
                target.write_text("{broken")
            elif case in ("npm-drift", "identity-drift", "excluded-runtime", "lifecycle-scripts"):
                package = json.loads(npm_path.read_text())
                if case == "npm-drift":
                    package["version"] = "0.0.1"
                elif case == "identity-drift":
                    package["name"] = "@invalid/cli"
                elif case == "excluded-runtime":
                    package["files"].remove("scripts/")
                else:
                    package["scripts"]["prepack"] = "node -e \"require('fs').writeFileSync('lifecycle-ran', 'bad')\""
                npm_path.write_text(json.dumps(package))
            elif case in ("server-drift", "registry-drift"):
                server = json.loads(server_path.read_text())
                target = server if case == "server-drift" else server["packages"][0]
                target["version"] = "0.0.1"
                server_path.write_text(json.dumps(server))
            elif case == "cargo-drift":
                (candidate / "Cargo.toml").write_text('[workspace.package]\nversion = "0.0.1"\n')
            elif case == "missing-runtime":
                (candidate / "npm/bin/orbit.js").rename(candidate / "removed-orbit.js")
            env = isolated_environment(temp / f"env-{case}")
            evidence = run(command, cwd=candidate, env=env)
            failure = None
            assertions = []
            try:
                assertions = validate_npm_result(candidate, command, evidence)
            except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
                failure = str(error)
            result = finalize_result(evidence, assertions, "fixture", failure)
            if case in ("valid", "lifecycle-scripts"):
                if result["outcome"] != "PASS" or len(assertions) != 2:
                    raise AssertionError(f"local candidate package failed: {result}")
                if (candidate / "npm/lifecycle-ran").exists():
                    raise AssertionError("candidate npm packaging executed a lifecycle script")
                # The retained tarball and metadata are independently checked on consumption.
                body = json.loads(evidence["stdout"])
                npm_pack_shape_self_test(candidate, body)
                body["archive"]["sha256"] = "0" * 64
                try:
                    validate_npm_evidence(candidate, body)
                except ValueError:
                    pass
                else:
                    raise AssertionError("altered archive evidence earned npm assertions")
            elif evidence["exit_code"] == 0 or result["outcome"] != "FAIL" or assertions:
                raise AssertionError(f"broken candidate earned npm assertions: {case}: {result}")
        predicate = run(["./scripts/smoke-npm-install.sh", "--dry-run-version-assertion"],
                        cwd=temp / "malformed-npm", env=env)
        try:
            validate_npm_result(temp / "malformed-npm",
                                ["./scripts/smoke-npm-install.sh", "--dry-run-version-assertion"], predicate)
        except ValueError:
            pass
        else:
            raise AssertionError("constant version self-test certified candidate packaging")
        if predicate["exit_code"] != 0:
            raise AssertionError("narrow version predicate self-test regressed")
        try:
            validate_npm_result(temp / "valid", command, predicate)
        except ValueError:
            pass
        else:
            raise AssertionError("exit-zero predicate output substituted for candidate evidence")


def self_test():
    process_self_test()
    builtin_init_self_test()
    npm_package_self_test()
    grouped = "Usage: orbit task <COMMAND>\n\nTasks:\n  add  Create task\nHealth:\n  recheck-blocked\n               Requeue\nOptions:\n  --json\nExamples:\n  orbit task add\n"
    if cli_help_children(grouped) != ["add", "recheck-blocked"]:
        raise AssertionError("grouped CLI help omitted commands or admitted examples")
    optional = "Usage: orbit doctor [OPTIONS] [COMMAND]\nCommands:\n  providers  Diagnose providers\n  fs-access  Inspect filesystem boundary\n"
    if cli_help_children(optional) != ["providers", "fs-access"]:
        raise AssertionError("optional CLI subcommands were omitted from discovery")
    continued = optional.replace("Usage: orbit doctor [OPTIONS] [COMMAND]", "Usage: orbit doctor [OPTIONS]\n       orbit doctor <COMMAND>")
    if cli_help_children(continued) != ["providers", "fs-access"]:
        raise AssertionError("continued CLI usage omitted focused doctor commands")
    if cli_help_children("Usage: orbit task show <ID>\nArguments:\n  id  Task ID\n"):
        raise AssertionError("leaf CLI arguments were mistaken for commands")
    command = ["cargo", "test", "--lib", "renamed_filter"]
    for output in ("", "running 0 tests\n",
                   "test result: ok. 0 passed; 0 failed; 2 ignored; 9 filtered out;",
                   "test result: FAILED. 0 passed; 1 failed; 0 ignored;"):
        try:
            validate_cargo_test_evidence(command, {"stdout": output, "stderr": ""})
        except ValueError:
            continue
        raise AssertionError(f"vacuous or failed cargo evidence passed: {output!r}")
    completed_tests = {"stdout": "test result: ok. 0 passed; 0 failed; 0 ignored;\n"
                                "test result: ok. 3 passed; 0 failed; 1 ignored;",
                       "stderr": ""}
    validate_cargo_test_evidence(command, completed_tests)
    if completed_tests["test_counts"] != {"passed": 3, "failed": 0, "ignored": 1, "suites": 2}:
        raise AssertionError("completed Rust test counts were not retained")
    unrelated = {"stdout": "test unrelated ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;",
                 "stderr": ""}
    try:
        validate_cargo_test_evidence(command, unrelated, ["required_behavior"])
    except ValueError:
        pass
    else:
        raise AssertionError("an unrelated passing test earned the scenario's assertions")
    named = {"stdout": "test required_behavior ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;",
             "stderr": ""}
    validate_cargo_test_evidence(command, named, ["required_behavior"])
    if named["passing_tests"] != ["required_behavior"]:
        raise AssertionError("named behavioral evidence was not retained")
    packages = {"fixture": [{"name": "boundary", "kind": ["test"]},
                            {"name": "fixture", "kind": ["lib"], "test": True}]}
    for selector in (["--test", "boundary"], ["--lib"]):
        validate_cargo_target(["cargo", "test", "-p", "fixture", *selector], packages)
    for command in (["cargo", "test", "-p", "fixture", "--test", "retired"],
                    ["cargo", "test", "-p", "missing", "--lib"],
                    ["cargo", "test", "-p", "fixture"],
                    ["cargo", "test", "--test", "boundary"]):
        try:
            validate_cargo_target(command, packages)
        except ValueError:
            continue
        raise AssertionError(f"invalid Cargo target selection passed: {command}")
    for listing in ({"exit_code": 0, "stdout": "0 tests, 0 benchmarks", "stderr": ""},
                    {"exit_code": 0, "stdout": "unrelated: test\n", "stderr": ""},
                    {"exit_code": 1, "stdout": "required_behavior: test\n", "stderr": "failed"},
                    {"exit_code": 0, "stdout": "required_behavior: test\n", "stderr": "",
                     "output_truncated": True}):
        try:
            validate_cargo_case_listing(listing, ["required_behavior"])
        except ValueError:
            continue
        raise AssertionError(f"invalid Cargo case discovery passed: {listing}")
    validate_cargo_case_listing({"exit_code": 0, "stdout": "module::required_behavior: test\n",
                                 "stderr": ""}, ["module::required_behavior"])
    inventory = {"scenarios": [{"id":"required", "required":True,
                                "assertions":["exact-json", "persisted-effect"]}],
                 "required_capability_gaps": {}}
    candidate = "candidate-a"
    valid = [{"scenario":"required", "outcome":"PASS", "candidate_id":candidate,
              "assertions":["exact-json", "persisted-effect"]}]
    if not scenario_decision(inventory, valid, candidate)[0]:
        raise AssertionError("valid evidence did not pass")
    mutations = [
        [{**valid[0], "assertions":[]}],
        [{**valid[0], "assertions":["exact-json"]}],
        [{**valid[0], "candidate_id":"stale-candidate"}],
        [valid[0], {**valid[0], "candidate_id":"stale-candidate"}],
        [{**valid[0], "outcome":"FAIL"}],
        [{**valid[0], "outcome":"NOT_RUN"}],
    ]
    for evidence in mutations:
        if scenario_decision(inventory, evidence, candidate)[0]:
            raise AssertionError(f"invalid evidence passed: {evidence}")
    with_gap = {**inventory, "required_capability_gaps":{"mcp:missing":"not exercised"}}
    if scenario_decision(with_gap, valid, candidate)[0]:
        raise AssertionError("required capability gap passed")
    coverage_gap = {"scenarios": [{**inventory["scenarios"][0], "kind": "coverage-gap",
                                   "coverage_gap": "retired case has no boundary replacement"}]}
    if scenario_decision(coverage_gap, valid, candidate)[0]:
        raise AssertionError("unrelated PASS evidence concealed a required behavioral gap")
    if scenario_decision(inventory, [], candidate)[0]:
        raise AssertionError("missing required scenario passed")
    before = {"workspace":"qa-primary", "artifacts":[]}
    wrong_workspace = {"workspace":"qa-other", "artifacts":[]}
    unchanged = {"workspace":"qa-primary", "artifacts":[]}
    if wrong_workspace["workspace"] == before["workspace"]:
        raise AssertionError("wrong-workspace self-test fixture is invalid")
    if unchanged["artifacts"] != before["artifacts"]:
        raise AssertionError("unchanged-persistence self-test fixture is invalid")
    for body, message in ((wrong_workspace, "wrong workspace"), (unchanged, "unchanged artifact")):
        try:
            if body["workspace"] != "qa-primary" or body["artifacts"] == before["artifacts"]:
                raise ValueError(message)
        except ValueError:
            continue
        raise AssertionError(f"{message} evidence passed")
    for stdout in ("", "{}", '{"wrong":true}'):
        evidence = {"exit_code":0, "stdout":stdout, "stderr":""}
        try:
            body = parse_json(evidence, "fake command")
            if body.get("expected") != "value":
                raise ValueError("wrong JSON")
        except ValueError:
            continue
        raise AssertionError(f"wrong or empty exit-zero output passed: {stdout!r}")

    with tempfile.TemporaryDirectory(prefix="orbit-qa-browser-self-test-") as tmp:
        temp = Path(tmp)
        browser_cache = temp / "browsers"
        browser_libs = temp / "sysroot"
        browser_cache.mkdir()
        browser_libs.mkdir()
        isolated = isolated_environment(temp, {
            "PATH": os.environ.get("PATH", ""), "ORBIT_ROOT": "must-not-leak",
            "PLAYWRIGHT_BROWSERS_PATH": "/host/browser-cache",
            "LD_LIBRARY_PATH": "/host/browser-libraries",
        })
        browser = browser_environment(isolated, browser_cache, browser_libs)
        if browser.get("PLAYWRIGHT_BROWSERS_PATH") != str(browser_cache):
            raise AssertionError("declared Playwright browser cache was dropped")
        if browser.get("LD_LIBRARY_PATH") != str(browser_libs):
            raise AssertionError("declared browser library path was dropped")
        if "ORBIT_ROOT" in isolated:
            raise AssertionError("isolated environment inherited host Orbit configuration")
        observed = run(["python3", "-c", "import json, os; print(json.dumps({key: os.getenv(key) for key in ['PLAYWRIGHT_BROWSERS_PATH', 'LD_LIBRARY_PATH', 'ORBIT_ROOT']}))"],
                       cwd=temp, env=browser)
        child = parse_json(observed, "browser environment regression")
        if child != {"PLAYWRIGHT_BROWSERS_PATH": str(browser_cache),
                     "LD_LIBRARY_PATH": str(browser_libs), "ORBIT_ROOT": None}:
            raise AssertionError(f"browser child environment is not bounded: {child!r}")
        evidence = temp / "retained-evidence"
        evidence.mkdir()
        (evidence / "failure.png").write_bytes(b"failure evidence")
        excluded = [str(evidence.relative_to(temp))]
        if not any(str(evidence.relative_to(temp)).startswith(path) for path in excluded):
            raise AssertionError("retained browser evidence is not excluded from candidate inputs")

    with tempfile.TemporaryDirectory(prefix="orbit-qa-platform-self-test-") as tmp:
        repo = Path(tmp)
        identity_paths = [
            "scripts/check-ci-macos.sh", "scripts/qa-full-sweep-inventory.json",
            "scripts/test-qa-full-sweep.py", ".github/workflows/ci-macos.yml",
        ]
        for relative in identity_paths:
            path = repo / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(relative + "\n")
        scenario = {
            "command": ["./scripts/check-ci-macos.sh"],
            "assertions": ["workflow-current", "matching-revision"],
        }
        hosted = {
            "evidence_type": "orbit-macos-platform", "platform": "macos",
            "source_revision": "a" * 40, "command": scenario["command"], "outcome": "PASS",
            "assertions": scenario["assertions"],
            "producer": {"system": "github-actions", "repository": "owner/orbit",
                         "run_id": "123", "run_attempt": "1",
                         "workflow_ref": "owner/orbit/.github/workflows/ci-macos.yml@refs/heads/main",
                         "workflow_sha": "b" * 40},
            "source_identity": {
                path: hashlib.sha256((repo / path).read_bytes()).hexdigest()
                for path in identity_paths
            },
        }
        candidate = {"head": "a" * 40, "status": []}
        validate_hosted_macos_evidence(hosted, scenario, candidate, repo)
        mutations = [
            {**hosted, "source_revision": "c" * 40},
            {**hosted, "command": ["./wrong-command"]},
            {**hosted, "outcome": "FAIL"},
            {**hosted, "outcome": "NOT_RUN"},
            {**hosted, "assertions": ["workflow-current"]},
            {**hosted, "producer": {**hosted["producer"], "run_id": ""}},
            {**hosted, "source_identity": {}},
        ]
        for mutation in mutations:
            try:
                validate_hosted_macos_evidence(mutation, scenario, candidate, repo)
            except ValueError:
                continue
            raise AssertionError(f"invalid hosted macOS evidence passed: {mutation!r}")
        try:
            validate_hosted_macos_evidence(hosted, scenario,
                                           {**candidate, "status": [" M scripts/file"]}, repo)
        except ValueError:
            pass
        else:
            raise AssertionError("hosted evidence passed for a dirty candidate")


def import_platform_evidence(paths, inventory, candidate, repo):
    imported = []
    candidate_id = candidate["candidate_id"]
    scenarios = {item["id"]: item for item in inventory["scenarios"]}
    for path in paths:
        body = json.loads(path.read_text())
        if body.get("schema_version") == 1:
            scenario = scenarios.get("macos-platform")
            if scenario is None:
                raise ValueError(f"{path}: macos-platform is absent from the inventory")
            validate_hosted_macos_evidence(body, scenario, candidate, repo)
            imported.append({
                "scenario": "macos-platform", "command": body["command"], "exit_code": 0,
                "stdout": json.dumps({"producer": body["producer"],
                                      "source_revision": body["source_revision"]}, sort_keys=True),
                "stderr": "", "outcome": "PASS", "assertions": body["assertions"],
                "candidate_id": candidate_id, "imported_from": str(path),
                "evidence_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            })
            continue
        if body.get("schema_version") != 2 or body.get("candidate", {}).get("id") != candidate_id:
            raise ValueError(f"{path}: evidence is for a different or unsupported candidate")
        for result in body.get("results", []):
            scenario = scenarios.get(result.get("scenario"))
            if not scenario:
                raise ValueError(f"{path}: imported scenario is not declared")
            if scenario.get("capability") == "local" or result.get("outcome") != "PASS":
                continue
            if result.get("command") != scenario.get("command"):
                raise ValueError(f"{path}: imported command does not match the required check")
            if result.get("candidate_id") != candidate_id:
                raise ValueError(f"{path}: result candidate does not match report candidate")
            imported.append({**result, "imported_from":str(path),
                             "evidence_sha256":hashlib.sha256(path.read_bytes()).hexdigest()})
    return imported


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--orbit-bin", default=shutil.which("orbit"))
    parser.add_argument("--output", type=Path)
    parser.add_argument("--run-commands", action="store_true")
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--check-cargo-selections", action="store_true",
                        help="with --check, compile and list each exact Cargo selection without executing it")
    parser.add_argument("--playwright-module", type=Path)
    parser.add_argument("--playwright-browsers-path", type=Path)
    parser.add_argument("--browser-ld-library-path", type=Path)
    parser.add_argument("--browser-evidence-dir", type=Path)
    parser.add_argument("--website-build", action="store_true")
    parser.add_argument("--build-candidate", action="store_true")
    parser.add_argument("--platform-evidence", action="append", type=Path, default=[])
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--list-cli-paths", action="store_true",
                        help="discover executable help paths without claiming behavioral coverage")
    args = parser.parse_args()
    repo = args.repo_root.resolve()
    inventory_path = repo / "scripts/qa-full-sweep-inventory.json"
    inventory = json.loads(inventory_path.read_text())
    errors, surfaces = validate_inventory(repo, inventory)
    if args.list_cli_paths:
        if not args.orbit_bin:
            parser.error("--list-cli-paths requires --orbit-bin")
        binary = str(Path(args.orbit_bin).resolve())
        with tempfile.TemporaryDirectory(prefix="orbit-qa-cli-grammar-") as tmp:
            temp = Path(tmp)
            paths = cli_command_paths(binary, surfaces["cli"], temp, isolated_environment(temp))
        print(json.dumps({"evidence_type": "help-grammar-discovery",
                          "behavioral_coverage": False, "command_paths": paths}, indent=2))
        return
    if args.self_test:
        self_test()
        print("qa-full-sweep self-tests: ok")
        return
    if args.check_cargo_selections and not args.check:
        parser.error("--check-cargo-selections requires --check")
    errors.extend(cargo_inventory_errors(repo, inventory, list_cases=args.check_cargo_selections))
    if args.check:
        if errors:
            print("\n".join(errors))
            raise SystemExit(1)
        self_test()
        print("qa-full-sweep inventory: ok")
        return
    output = (args.output or repo / "qa-full-sweep-report.json").resolve()
    browser_evidence_dir = (args.browser_evidence_dir or
                            output.parent / f"{output.stem}.browser-evidence").resolve()
    excluded_paths = []
    try:
        excluded_paths.append(output.relative_to(repo))
    except ValueError:
        pass
    try:
        excluded_paths.append(browser_evidence_dir.relative_to(repo))
    except ValueError:
        pass
    candidate = candidate_source(repo, excluded_paths)
    candidate_id = candidate["candidate_id"]
    results = [{"scenario":"inventory-guard", "command":[str(Path(__file__).relative_to(repo))],
                "exit_code":0 if not errors else 1, "stdout":json.dumps(surfaces, sort_keys=True),
                "stderr":"\n".join(errors), "outcome":"PASS" if not errors else "FAIL",
                "assertions":["all-source-surfaces-explicitly-covered-or-gapped"],
                "candidate_id":candidate_id}]

    builtin_halt = None
    binary = None if args.build_candidate else (Path(args.orbit_bin).resolve() if args.orbit_bin else None)
    browser_inputs_valid = bool(args.playwright_module and args.playwright_module.is_file()
                                and args.playwright_browsers_path and args.playwright_browsers_path.is_dir()
                                and args.browser_ld_library_path and args.browser_ld_library_path.is_dir())
    capabilities = {"local": bool(binary) or args.build_candidate,
                    "browser": browser_inputs_valid,
                    "website-build": args.website_build,
                    "macos": platform.system() == "Darwin"}
    binary_record = {"path":str(binary) if binary else None, "version":None,
                     "sha256":hashlib.sha256(binary.read_bytes()).hexdigest() if binary else None}
    browser_record = {
        "playwright_module": str(args.playwright_module) if args.playwright_module else None,
        "playwright_module_sha256": (hashlib.sha256(args.playwright_module.read_bytes()).hexdigest()
                                      if args.playwright_module and args.playwright_module.is_file() else None),
        "playwright_browsers_path": str(args.playwright_browsers_path) if args.playwright_browsers_path else None,
        "browser_ld_library_path": str(args.browser_ld_library_path) if args.browser_ld_library_path else None,
        "version": None,
        "capability_probe": None,
    }
    with tempfile.TemporaryDirectory(prefix="orbit-qa-full-") as tmp:
        temp = Path(tmp)
        env = isolated_environment(temp)
        browser_env = browser_environment(env, args.playwright_browsers_path,
                                          args.browser_ld_library_path)
        if browser_inputs_valid:
            probe, browser_record["version"] = inspect_browser_capability(
                args.playwright_module, browser_env, repo)
            browser_record["capability_probe"] = {
                "command": probe["command"], "exit_code": probe["exit_code"],
                "stdout": probe["stdout"], "stderr": probe["stderr"],
            }
            capabilities["browser"] = probe["exit_code"] == 0
        provenance = None
        if args.build_candidate and not errors:
            build = run(["cargo", "build", "--locked", "-p", "orbit-cli", "--bin", "orbit",
                         "--target-dir", str(temp / "candidate-target")], cwd=repo, env=env, timeout=1800)
            binary = temp / "candidate-target/debug/orbit"
            unchanged = candidate_source(repo, excluded_paths)["candidate_id"] == candidate_id
            failure = None if build["exit_code"] == 0 and binary.is_file() and unchanged else "candidate build failed or source changed during build"
            provenance = finalize_result(build,
                ["binary-built-from-current-source", "source-stable-through-build", "binary-hash-recorded"],
                candidate_id, failure)
            provenance["scenario"] = "source-binary-provenance"
            results.append(provenance)
        elif binary:
            evidence = run([str(binary), "--version"], cwd=repo, env=env)
            results.append({"scenario":"source-binary-provenance",
                **finalize_result(evidence, [], candidate_id,
                    "external binary has no build attestation; use --build-candidate")})
        if binary and not errors:
            binary_record = {"path":"<disposable-candidate-build>",
                             "version":run([str(binary), "--version"], cwd=repo, env=env)["stdout"].strip(),
                             "sha256":hashlib.sha256(binary.read_bytes()).hexdigest()}
            builtin_results, builtin_halt = run_builtins(repo, str(binary), temp, env, candidate_id)
            results.extend(builtin_results)
        for scenario in inventory["scenarios"]:
            if scenario["kind"] == "builtin" or scenario["id"] == "source-binary-provenance":
                continue
            if scenario["kind"] == "coverage-gap":
                results.append({"scenario": scenario["id"], "command": [], "exit_code": None,
                                "stdout": "", "stderr": scenario["coverage_gap"],
                                "outcome": "BLOCKED", "assertions": [], "candidate_id": candidate_id})
                continue
            if args.run_commands and capabilities.get(scenario["capability"], False):
                command = [part.replace("{playwright_module}", str(args.playwright_module))
                           .replace("{evidence_dir}", str(browser_evidence_dir))
                           for part in scenario["command"]]
                scenario_env = browser_env if scenario["capability"] == "browser" else env
                evidence = run(command, cwd=repo, env=scenario_env, timeout=1800)
                if scenario["capability"] == "browser":
                    browser_evidence_dir.mkdir(parents=True, exist_ok=True)
                    (browser_evidence_dir / "harness-result.json").write_text(
                        json.dumps({"command": command, "exit_code": evidence["exit_code"],
                                    "stdout": evidence["stdout"], "stderr": evidence["stderr"],
                                    "browser_version": browser_record["version"]}, indent=2) + "\n")
                unchanged = candidate_source(repo, excluded_paths)["candidate_id"] == candidate_id
                failure = None if unchanged else "source candidate changed while the scenario ran"
                assertions = scenario["assertions"]
                try:
                    validate_cargo_test_evidence(command, evidence, scenario.get("required_tests", []))
                    if scenario["id"] == "npm-package":
                        assertions = []
                        assertions = validate_npm_result(repo, command, evidence)
                except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
                    failure = str(error) if failure is None else failure + "; " + str(error)
                if scenario["id"] == "npm-package" and failure:
                    assertions = []
                result = {"scenario":scenario["id"],
                          **finalize_result(evidence, assertions, candidate_id, failure)}
                if scenario["capability"] == "browser":
                    artifacts = []
                    if browser_evidence_dir.is_dir():
                        for path in sorted(browser_evidence_dir.rglob("*")):
                            if path.is_file():
                                artifacts.append({"path": str(path.relative_to(browser_evidence_dir)),
                                                  "sha256": hashlib.sha256(path.read_bytes()).hexdigest()})
                    result["retained_evidence"] = {"directory": str(browser_evidence_dir),
                                                   "files": artifacts}
                results.append(result)
            else:
                results.append({"scenario":scenario["id"], "command":scenario.get("command", []),
                                "exit_code":None, "stdout":"", "stderr":f"capability or execution not enabled: {scenario['capability']}",
                                "outcome":"NOT_RUN", "assertions":[], "candidate_id":candidate_id})

        try:
            imported = import_platform_evidence(args.platform_evidence, inventory, candidate, repo)
            imported_scenarios = {result["scenario"] for result in imported}
            results = [result for result in results
                       if not (result["scenario"] in imported_scenarios and result["outcome"] == "NOT_RUN")]
            results.extend(imported)
        except (OSError, ValueError, json.JSONDecodeError) as error:
            results.append({"scenario":"platform-evidence-import", "command":[], "exit_code":1,
                            "stdout":"", "stderr":str(error), "outcome":"FAIL",
                            "assertions":[], "candidate_id":candidate_id})

    covered = {result["scenario"] for result in results}
    for scenario in inventory["scenarios"]:
        if scenario["id"] not in covered:
            results.append({"scenario":scenario["id"], "command":scenario.get("command", []),
                            "exit_code":None, "stdout":"", "stderr":builtin_halt or "builtin did not reach scenario",
                            "outcome":"NOT_RUN", "assertions":[], "candidate_id":candidate_id})
    full_pass, decision_failures = scenario_decision(inventory, results, candidate_id)
    full_pass = full_pass and not errors
    changed_paths = [line[3:] for line in candidate["status"] if " -> " not in line[3:]]
    changed_hashes = {
        path: hashlib.sha256((repo / path).read_bytes()).hexdigest()
        for path in changed_paths if (repo / path).is_file()
    }
    report = {
        "schema_version": 2, "generated_at": datetime.now(timezone.utc).isoformat(),
        "candidate": {"id":candidate_id, "source_revision":candidate["head"]},
        "source_state": {"status": candidate["status"], "diff_sha256": candidate["diff_sha256"],
                         "changed_file_sha256": changed_hashes},
        "binary": binary_record, "browser": browser_record,
        "managed_assets": {str(path.relative_to(repo)):hashlib.sha256(path.read_bytes()).hexdigest()
                           for path in repo.glob(".orbit/**/.orbit-managed-assets.json")},
        "environment": {"os":platform.system(), "release":platform.release(), "architecture":platform.machine(),
                        "capabilities":capabilities},
        "inventory_sha256": hashlib.sha256(inventory_path.read_bytes()).hexdigest(),
        "results": results, "findings": decision_failures,
        "post_publish_handoff": {"npm_freshness":"PENDING", "website_freshness":"PENDING",
                                  "part_of_pre_release_decision":False},
        "decision":"PASS" if full_pass else "INCOMPLETE"
    }
    output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(f"qa-full-sweep: {report['decision']} ({output})")
    if errors:
        raise SystemExit(1)


if __name__ == "__main__":
    main()

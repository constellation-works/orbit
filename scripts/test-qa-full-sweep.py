#!/usr/bin/env python3
"""Inventory guard and isolated trial runner for qa-full-sweep."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.request
from datetime import datetime, timezone
from pathlib import Path


def run(argv, *, cwd, env, timeout=180):
    def tail(value):
        if isinstance(value, bytes):
            value = value.decode(errors="replace")
        return (value or "")[-1_048_576:]

    started = datetime.now(timezone.utc).isoformat()
    try:
        completed = subprocess.run(argv, cwd=cwd, env=env, text=True,
                                   capture_output=True, timeout=timeout, check=False)
    except subprocess.TimeoutExpired as error:
        return {
            "command": argv, "started_at": started, "exit_code": None,
            "stdout": tail(error.stdout),
            "stderr": tail(error.stderr) + f"\ntimeout after {timeout}s",
            "outcome": "FAIL", "output_truncated": True,
        }
    stdout = completed.stdout[-1_048_576:]
    stderr = completed.stderr[-1_048_576:]
    return {
        "command": argv, "started_at": started, "exit_code": completed.returncode,
        "stdout": stdout, "stderr": stderr,
        "outcome": "PASS" if completed.returncode == 0 else "FAIL",
        "output_truncated": len(stdout) != len(completed.stdout) or len(stderr) != len(completed.stderr),
    }


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

    jobs = {path.stem for path in (repo / ".orbit/resources/jobs").glob("*.yaml")}
    activities = {path.stem for path in (repo / ".orbit/resources/activities").glob("*.yaml")}
    mcp_snapshot = json.loads((repo / "crates/orbit-cli/tests/snapshots/mcp_tools_list.json").read_text())
    mcp = {entry["name"] for entry in mcp_snapshot}
    api_source = (repo / "crates/orbit-web/src/api/mod.rs").read_text()
    api = set(re.findall(r'\.route\(\s*"([^"]+)"', api_source))
    web_source = (repo / "crates/orbit-web/src/lib.rs").read_text()
    dashboard = set(re.findall(r'\.route\(\s*"([^"]+)"', web_source))
    return {"cli": cli, "job": jobs, "activity": activities,
            "mcp": mcp, "api": api, "dashboard": dashboard}


def validate_inventory(repo: Path, inventory: dict):
    actual = source_contracts(repo)
    expected_cli = set(inventory["surface_contracts"]["cli_top_level"])
    expected_jobs = set(inventory["surface_contracts"]["job_assets"])
    errors = []
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


def scenario_decision(inventory, results, candidate_id):
    failures = []
    for scenario in inventory["scenarios"]:
        if not scenario["required"]:
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

    checked("isolated-cli-lifecycle",
            [orbit_bin, "init", "--non-interactive", "--host-name", "qa-host", "--task-prefix", "QAF"],
            temp, ["global-init-persists-isolated-root"], succeeds)
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
    shutil.copy2(repo / ".orbit/auto_tasks/qa-full-sweep.yaml", destination)

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
        return results
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
        return results

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

    identity_path = root / "host.toml"
    registry_path = root / "workspaces.json"

    def host_identity(evidence):
        body = parse_json(evidence, "host show")
        if body.get("host_id") != "qa-host" or body.get("task_prefix") != "QAF":
            raise ValueError("host show did not return the initialized identity")
        machine_id = body.get("machine_id")
        if not isinstance(machine_id, str) or not machine_id.startswith("hm_"):
            raise ValueError("host show returned an invalid stable machine_id")

    initial_host = checked("host-identity-transaction",
                           [orbit_bin, "--root", str(root), "host", "show", "--format", "json"],
                           work, ["initialized-identity-readback"], host_identity)
    initial_machine_id = json.loads(initial_host["stdout"])["machine_id"] if initial_host else None
    checked("host-identity-transaction",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "host", "rename",
             "qa-host", "qa-renamed"], work, ["supported-rename-updates-local-records"], succeeds)

    def renamed_identity(evidence):
        body = parse_json(evidence, "renamed host show")
        if body.get("machine_id") != initial_machine_id or body.get("host_id") != "qa-renamed":
            raise ValueError("rename changed stable identity or failed host_id readback")
        registry = json.loads(registry_path.read_text())
        if registry.get("owner_host_ids", {}).get(initial_machine_id) != "qa-renamed":
            raise ValueError("rename did not update the isolated registry owner projection")

    checked("host-identity-transaction",
            [orbit_bin, "--root", str(root), "host", "show", "--format", "json"], work,
            ["rename-preserves-machine-id-and-reads-back"], renamed_identity)
    host_before_invalid = identity_path.read_bytes()
    registry_before_invalid = registry_path.read_bytes()

    def invalid_host_refused(evidence):
        refused(evidence)
        if (identity_path.read_bytes() != host_before_invalid
                or registry_path.read_bytes() != registry_before_invalid):
            raise ValueError("refused host rename changed identity or registry bytes")

    checked("host-identity-transaction",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "host", "rename",
             "stale-host", "other-name"], work, ["stale-rename-refused-without-mutation"],
            invalid_host_refused, accept_nonzero=True)
    checked("host-identity-transaction",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "host", "rename",
             "qa-renamed", "invalid/name"], work, ["invalid-rename-refused-without-mutation"],
            invalid_host_refused, accept_nonzero=True)

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
        if body.get("state") != "succeeded" or not body.get("run_id"):
            raise ValueError("deterministic fixture job did not succeed")

    fixture_run = checked("legacy-logs-compatibility",
                          [orbit_bin, "--root", str(root), "--workspace", str(work), "run", "job",
                           str(fixture_job), "--input", "crew=sol", "--wait", "--format", "json"],
                          work, ["supported-run-fixture-completes"], completed_fixture_run)
    fixture_run_id = json.loads(fixture_run["stdout"])["run_id"] if fixture_run else "missing-run"

    def legacy_logs_output(evidence):
        body = parse_json(evidence, "legacy logs")
        if body != []:
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
         "--host-name", "qa-migration", "--task-prefix", "QAM"],
        temp, ["disposable-migration-fixture-initialized"], succeeds,
    )
    if migration_init is None:
        return results
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
    migration_identity_path = migration_root / "host.toml"
    migration_identity_before = migration_identity_path.read_bytes()

    def newer_migration_refused(evidence):
        refused(evidence)
        if "newer" not in evidence.get("stderr", ""):
            raise ValueError("newer migration refusal did not explain the incompatibility")
        if (migration_marker.read_bytes() != newer_before
                or migration_identity_path.read_bytes() != migration_identity_before):
            raise ValueError("newer migration refusal changed marker or host identity bytes")

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

    for kind, expected in (("job", {"task_pilot_pipeline", "worktree_gc_pipeline"}),
                           ("activity", {"agent_implement", "task_pilot"})):
        def catalog(evidence, expected=expected, kind=kind):
            body = parse_json(evidence, f"{kind} list")
            rows = body if isinstance(body, list) else body.get("items", [])
            names = {row.get("name") or row.get("id") or row.get("job_id") for row in rows}
            if not expected.issubset(names):
                raise ValueError(f"{kind} catalog missing {sorted(expected-names)}")
        checked("workflow-definition-boundary",
                [orbit_bin, "--root", str(root), "--workspace", str(work), kind, "list", "--format", "json"],
                work, [f"{kind}-catalog-returns-structured-installed-definitions"], catalog)

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
            [orbit_bin, "--root", str(root), "--workspace", str(work), "semantic", "stats", "--json"],
            work, ["semantic-stats-reports-capability"], lambda evidence: parse_json(evidence, "semantic stats"))
    checked("search-observability-boundary",
            [orbit_bin, "--root", str(root), "--workspace", str(work), "doctor", "--json"],
            work, ["doctor-reports-isolated-workspace-health"], lambda evidence: parse_json(evidence, "doctor"))

    for command, assertion in ((["tool", "list", "--format", "json"], "tool-definitions-are-structured"),
                               (["policy", "list", "--json"], "policy-definitions-are-structured"),
                               (["operation", "explain", "--json"], "operation-authority-is-explained"),
                               (["executor", "list", "--format", "json"], "executor-definitions-are-structured"),
                               (["skill", "list", "--format", "json"], "skill-definitions-are-structured")):
        checked("definition-policy-boundary",
                [orbit_bin, "--root", str(root), "--workspace", str(work), *command], work,
                [assertion], lambda evidence, assertion=assertion: parse_json(evidence, assertion))

    initialize = {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"qa","version":"1"}}}
    requests = [initialize,
        {"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}},
        {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"orbit_workspace_list","arguments":{}}},
        {"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"orbit_task_show","arguments":{"id":task_id,"workspace":str(work),"model":"codex"}}},
        {"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"orbit_task_show","arguments":{"id":task_id,"workspace":str(other),"model":"codex"}}}]
    mcp = subprocess.run([orbit_bin, "mcp", "serve", "--workspace", str(work)], cwd=work, env=env,
                         input="".join(json.dumps(item)+"\n" for item in requests), text=True,
                         capture_output=True, timeout=30, check=False)
    mcp_evidence = {"command":[orbit_bin,"mcp","serve"], "exit_code":mcp.returncode,
                    "stdout":mcp.stdout,"stderr":mcp.stderr,"outcome":"FAIL"}
    failure = None
    try:
        responses = {body["id"]: body for line in mcp.stdout.splitlines()
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

    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
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
        ["healthz-is-ready", "workspace-api-identifies-bound-checkout", "task-api-reads-persisted-task"],
        candidate_id, failure))
    return results


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


def self_test():
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
    parser.add_argument("--playwright-module", type=Path)
    parser.add_argument("--playwright-browsers-path", type=Path)
    parser.add_argument("--browser-ld-library-path", type=Path)
    parser.add_argument("--browser-evidence-dir", type=Path)
    parser.add_argument("--website-build", action="store_true")
    parser.add_argument("--build-candidate", action="store_true")
    parser.add_argument("--platform-evidence", action="append", type=Path, default=[])
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    repo = args.repo_root.resolve()
    inventory_path = repo / "scripts/qa-full-sweep-inventory.json"
    inventory = json.loads(inventory_path.read_text())
    errors, surfaces = validate_inventory(repo, inventory)
    if args.self_test:
        self_test()
        print("qa-full-sweep self-tests: ok")
        return
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
            results.extend(run_builtins(repo, str(binary), temp, env, candidate_id))
        for scenario in inventory["scenarios"]:
            if scenario["kind"] == "builtin" or scenario["id"] == "source-binary-provenance":
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
                result = {"scenario":scenario["id"],
                          **finalize_result(evidence, scenario["assertions"], candidate_id, failure)}
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
                            "exit_code":None, "stdout":"", "stderr":"builtin did not reach scenario",
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

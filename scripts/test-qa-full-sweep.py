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
    excluded = {str(path) for path in excluded_paths}
    status_lines = [line for line in status.splitlines()
                    if (line[3:].split(" -> ")[-1] if len(line) > 3 else line) not in excluded]
    untracked_output = subprocess.check_output(
        ["git", "ls-files", "--others", "--exclude-standard", "-z"], cwd=repo)
    untracked = []
    for raw_path in untracked_output.split(b"\0"):
        if not raw_path:
            continue
        path = raw_path.decode()
        candidate = repo / path
        if path not in excluded and candidate.is_file():
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


def isolated_environment(temp: Path):
    keep = ["PATH", "USER", "LOGNAME", "LANG", "LC_ALL", "RUSTUP_TOOLCHAIN"]
    env = {key: os.environ[key] for key in keep if key in os.environ}
    user_home = Path.home()
    env.update({"HOME": str(temp / "home"), "USERPROFILE": str(temp / "home"),
                "XDG_CONFIG_HOME": str(temp / "xdg"), "TMPDIR": str(temp / "tmp"),
                "CARGO_HOME": os.environ.get("CARGO_HOME", str(user_home / ".cargo")),
                "RUSTUP_HOME": os.environ.get("RUSTUP_HOME", str(user_home / ".rustup"))})
    for path in (temp / "home", temp / "xdg", temp / "tmp"):
        path.mkdir(parents=True, exist_ok=True)
    return env


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
        except (AssertionError, KeyError, TypeError, ValueError) as error:
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


def import_platform_evidence(paths, inventory, candidate_id):
    imported = []
    scenarios = {item["id"]: item for item in inventory["scenarios"]}
    for path in paths:
        body = json.loads(path.read_text())
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
    excluded_paths = []
    try:
        excluded_paths.append(output.relative_to(repo))
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
    capabilities = {"local": bool(binary) or args.build_candidate,
                    "browser": bool(args.playwright_module and args.playwright_module.is_file()),
                    "website-build": args.website_build,
                    "macos": platform.system() == "Darwin"}
    binary_record = {"path":str(binary) if binary else None, "version":None,
                     "sha256":hashlib.sha256(binary.read_bytes()).hexdigest() if binary else None}
    with tempfile.TemporaryDirectory(prefix="orbit-qa-full-") as tmp:
        temp = Path(tmp)
        env = isolated_environment(temp)
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
                           .replace("{evidence_dir}", str(temp / "browser-evidence"))
                           for part in scenario["command"]]
                evidence = run(command, cwd=repo, env=env, timeout=1800)
                unchanged = candidate_source(repo, excluded_paths)["candidate_id"] == candidate_id
                failure = None if unchanged else "source candidate changed while the scenario ran"
                results.append({"scenario":scenario["id"],
                    **finalize_result(evidence, scenario["assertions"], candidate_id, failure)})
            else:
                results.append({"scenario":scenario["id"], "command":scenario.get("command", []),
                                "exit_code":None, "stdout":"", "stderr":f"capability or execution not enabled: {scenario['capability']}",
                                "outcome":"NOT_RUN", "assertions":[], "candidate_id":candidate_id})

        try:
            imported = import_platform_evidence(args.platform_evidence, inventory, candidate_id)
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
        "binary": binary_record,
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

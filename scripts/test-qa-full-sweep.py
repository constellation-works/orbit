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


def mapped(surface: str, patterns: list[str]):
    for pattern in patterns:
        if pattern.endswith("*") and surface.startswith(pattern[:-1]):
            return True
        if "*" in pattern:
            regex = "^" + re.escape(pattern).replace(r"\*", ".*") + "$"
            if re.match(regex, surface):
                return True
        if surface == pattern:
            return True
    return False


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
    patterns = []
    for family in inventory["feature_families"]:
        patterns.extend(family["surfaces"])
        missing = set(family["scenarios"]) - scenario_ids
        if missing:
            errors.append(f"family {family['id']} names missing scenarios {sorted(missing)}")
    for kind, entries in actual.items():
        for entry in entries:
            surface = f"{kind}:{entry}"
            if not mapped(surface, patterns):
                errors.append(f"unmapped surface {surface}")
    required_behaviors = {"normal", "failure"}
    for scenario in inventory["scenarios"]:
        if not required_behaviors.issubset(set(scenario["behavior"])):
            errors.append(f"scenario {scenario['id']} lacks normal/failure coverage")
    return errors, {kind: sorted(entries) for kind, entries in actual.items()}


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


def run_builtins(repo: Path, orbit_bin: str, inventory: dict, temp: Path, env: dict):
    results = []
    root = temp / "home/.orbit"
    work = temp / "workspace"
    other = temp / "other-workspace"
    for path in (work, other):
        path.mkdir()
        run(["git", "init", "-q", "-b", "main"], cwd=path, env=env)
        (path / "README.md").write_text("fixture\n")
        run(["git", "add", "README.md"], cwd=path, env=env)
        run(["git", "-c", "user.name=Orbit QA", "-c", "user.email=qa@example.invalid",
             "commit", "-qm", "fixture"], cwd=path, env=env)

    commands = [
        ("isolated-cli-lifecycle", [orbit_bin, "init", "--non-interactive", "--host-name", "qa-host", "--task-prefix", "QAF"] , temp),
        ("isolated-cli-lifecycle", [orbit_bin, "workspace", "init", "--name", "qa-primary"], work),
        ("workspace-boundary", [orbit_bin, "workspace", "init", "--name", "qa-other"], other),
        ("workspace-boundary", [orbit_bin, "--root", str(root), "--workspace", str(work), "workspace", "show", "--format", "json"], other),
    ]
    for scenario, argv, cwd in commands:
        add_result(results, scenario, run(argv, cwd=cwd, env=env))
    if any(item["outcome"] == "FAIL" for item in results):
        return results

    destination = work / ".orbit/auto_tasks/qa-full-sweep.yaml"
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(repo / ".orbit/auto_tasks/qa-full-sweep.yaml", destination)
    task = run([orbit_bin, "--root", str(root), "--workspace", str(work), "task", "add",
                "--title", "QA fixture task", "--complexity", "low", "--status", "backlog",
                "--acceptance-criteria", "fixture round trip", "--json"], cwd=work, env=env)
    add_result(results, "isolated-cli-lifecycle", task)
    if task["outcome"] == "PASS":
        task_id = json.loads(task["stdout"])["id"]
        payload = temp / "artifact.txt"
        payload.write_text("qa evidence\n")
        for argv in ([orbit_bin, "--root", str(root), "--workspace", str(work), "task", "artifact", "put", task_id, str(payload), "--json"],
                     [orbit_bin, "--root", str(root), "--workspace", str(work), "task", "show", task_id, "--json"],
                     [orbit_bin, "--root", str(root), "--workspace", str(work), "task", "list", "--limit", "1", "--json"]):
            add_result(results, "isolated-cli-lifecycle", run(argv, cwd=work, env=env))

    for scenario, argv in [
        ("auto-task-registration-mint", [orbit_bin, "--root", str(root), "--workspace", str(work), "auto-task", "show", "qa-full-sweep", "--format", "json"]),
        ("auto-task-registration-mint", [orbit_bin, "--root", str(root), "--workspace", str(work), "auto-task", "mint", "qa-full-sweep", "--json"]),
        ("workflow-catalog", [orbit_bin, "--root", str(root), "--workspace", str(work), "job", "list", "--format", "json"]),
        ("workflow-catalog", [orbit_bin, "--root", str(root), "--workspace", str(work), "activity", "list", "--format", "json"]),
        ("docs-search-observe", [orbit_bin, "--root", str(root), "--workspace", str(work), "search", "fixture", "--format", "json"]),
        ("definition-surfaces", [orbit_bin, "--root", str(root), "--workspace", str(work), "tool", "list", "--format", "json"]),
    ]:
        add_result(results, scenario, run(argv, cwd=work, env=env))

    initialize = json.dumps({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"qa","version":"1"}}})
    listed = json.dumps({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}})
    mcp = subprocess.run([orbit_bin, "mcp", "serve", "--workspace", str(work)],
                         cwd=work, env=env, input=initialize+"\n"+listed+"\n", text=True,
                         capture_output=True, timeout=30, check=False)
    add_result(results, "mcp-wire-boundary", {"command":[orbit_bin,"mcp","serve"],
        "exit_code":mcp.returncode,"stdout":mcp.stdout,"stderr":mcp.stderr,
        "outcome":"PASS" if mcp.returncode == 0 and "orbit_task_show" in mcp.stdout else "FAIL"})

    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    web = subprocess.Popen([orbit_bin, "--root", str(root), "web", "serve", "--host", "127.0.0.1", "--port", str(port), "--no-open"],
                           cwd=work, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    web_evidence = {"command":[orbit_bin,"web","serve"], "exit_code":1, "stdout":"", "stderr":"dashboard did not become ready", "outcome":"FAIL"}
    try:
        for _ in range(50):
            if web.poll() is not None:
                break
            try:
                health = urllib.request.urlopen(f"http://127.0.0.1:{port}/healthz", timeout=1).read().decode()
                tasks = urllib.request.urlopen(f"http://127.0.0.1:{port}/api/workspaces", timeout=1).read().decode()
                web_evidence = {"command":["GET","/healthz","GET","/api/workspaces"], "exit_code":0,
                                "stdout":health+"\n"+tasks, "stderr":"", "outcome":"PASS"}
                break
            except Exception:
                time.sleep(.1)
    finally:
        web.terminate()
        try:
            web.wait(timeout=5)
        except subprocess.TimeoutExpired:
            web.kill()
        captured_stdout, captured_stderr = web.communicate()
        if web_evidence["outcome"] == "FAIL":
            web_evidence["stdout"] = captured_stdout
            web_evidence["stderr"] = captured_stderr or web_evidence["stderr"]
    add_result(results, "dashboard-api-boundary", web_evidence)
    return results


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--orbit-bin", default=shutil.which("orbit"))
    parser.add_argument("--output", type=Path)
    parser.add_argument("--run-commands", action="store_true")
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--playwright-module", type=Path)
    parser.add_argument("--website-build", action="store_true")
    args = parser.parse_args()
    repo = args.repo_root.resolve()
    inventory_path = repo / "scripts/qa-full-sweep-inventory.json"
    inventory = json.loads(inventory_path.read_text())
    errors, surfaces = validate_inventory(repo, inventory)
    if args.check:
        if errors:
            print("\n".join(errors))
            raise SystemExit(1)
        print("qa-full-sweep inventory: ok")
        return
    results = [{"scenario":"inventory-guard", "command":[str(Path(__file__).relative_to(repo))],
                "exit_code":0 if not errors else 1, "stdout":json.dumps(surfaces, sort_keys=True),
                "stderr":"\n".join(errors), "outcome":"PASS" if not errors else "FAIL"}]

    binary = Path(args.orbit_bin).resolve() if args.orbit_bin else None
    capabilities = {"local": bool(binary),
                    "browser": bool(args.playwright_module and args.playwright_module.is_file()),
                    "website-build": args.website_build,
                    "macos": platform.system() == "Darwin"}
    with tempfile.TemporaryDirectory(prefix="orbit-qa-full-") as tmp:
        temp = Path(tmp)
        env = isolated_environment(temp)
        if binary and not errors:
            results.extend(run_builtins(repo, str(binary), inventory, temp, env))
        for scenario in inventory["scenarios"]:
            if scenario["kind"] == "builtin":
                continue
            if scenario["id"] == "source-binary-provenance" and binary:
                results.append({"scenario":scenario["id"], **run([str(binary), "--version"], cwd=repo, env=env)})
            elif args.run_commands and capabilities.get(scenario["capability"], False):
                command = [part.replace("{playwright_module}", str(args.playwright_module))
                           .replace("{evidence_dir}", str(temp / "browser-evidence"))
                           for part in scenario["command"]]
                results.append({"scenario":scenario["id"], **run(command, cwd=repo, env=env, timeout=1800)})
            else:
                results.append({"scenario":scenario["id"], "command":scenario.get("command", []),
                                "exit_code":None, "stdout":"", "stderr":f"capability or execution not enabled: {scenario['capability']}",
                                "outcome":"NOT_RUN"})

    covered = {result["scenario"] for result in results}
    for scenario in inventory["scenarios"]:
        if scenario["id"] not in covered:
            results.append({"scenario":scenario["id"], "command":scenario.get("command", []),
                            "exit_code":None, "stdout":"", "stderr":"builtin did not reach scenario", "outcome":"NOT_RUN"})
    required = {item["id"] for item in inventory["scenarios"] if item["required"]}
    full_pass = not errors and all(
        any(r["scenario"] == sid for r in results)
        and all(r["outcome"] == "PASS" for r in results if r["scenario"] == sid)
        for sid in required
    )
    diff = subprocess.check_output(["git", "diff", "--binary", "HEAD"], cwd=repo)
    status = subprocess.check_output(["git", "status", "--short"], cwd=repo, text=True)
    changed_paths = [line[3:] for line in status.splitlines() if " -> " not in line[3:]]
    changed_hashes = {
        path: hashlib.sha256((repo / path).read_bytes()).hexdigest()
        for path in changed_paths if (repo / path).is_file()
    }
    binary_version = run([str(binary), "--version"], cwd=repo, env=os.environ)["stdout"].strip() if binary else None
    report = {
        "schema_version": 1, "generated_at": datetime.now(timezone.utc).isoformat(),
        "source_revision": subprocess.check_output(["git","rev-parse","HEAD"], cwd=repo, text=True).strip(),
        "source_state": {"status": status.splitlines(), "diff_sha256": hashlib.sha256(diff).hexdigest(),
                         "changed_file_sha256": changed_hashes},
        "binary": {"path":str(binary) if binary else None,
                   "version": binary_version,
                   "sha256":hashlib.sha256(binary.read_bytes()).hexdigest() if binary else None},
        "managed_assets": {str(path.relative_to(repo)):hashlib.sha256(path.read_bytes()).hexdigest()
                           for path in repo.glob(".orbit/**/.orbit-managed-assets.json")},
        "environment": {"os":platform.system(), "release":platform.release(), "architecture":platform.machine(),
                        "capabilities":capabilities},
        "inventory_sha256": hashlib.sha256(inventory_path.read_bytes()).hexdigest(),
        "results": results, "findings": [],
        "decision":"PASS" if full_pass else "INCOMPLETE"
    }
    output = args.output or repo / "qa-full-sweep-report.json"
    output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(f"qa-full-sweep: {report['decision']} ({output})")
    if errors:
        raise SystemExit(1)


if __name__ == "__main__":
    main()

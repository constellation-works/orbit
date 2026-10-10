#!/usr/bin/env python3
"""Prepare an isolated MCP Apps candidate and record protocol (never native) evidence."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import sys
import threading

ROOT = Path(__file__).resolve().parents[1]
URI = "ui://orbit/control-center/v1/index.html"
LEGACY_URI = "ui://orbit/task-panel/v1/index.html"


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path, help="Already-built candidate orbit binary")
    parser.add_argument("--output", type=Path, default=ROOT / ".orbit/tmp/mcp-apps-probe")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    output = args.output.resolve()
    scratch = (ROOT / ".orbit/tmp").resolve()
    if not scratch.is_relative_to(ROOT) or not output.is_relative_to(scratch) or output == scratch:
        parser.error("output must be a new directory beneath this checkout's .orbit/tmp")
    ancestor = output.parent
    while not ancestor.exists():
        ancestor = ancestor.parent
    usage = shutil.disk_usage(ancestor)
    if usage.used / usage.total >= 0.80:
        parser.error("disk usage is >=80%; no fixture created")
    parent = ROOT
    for part in output.parent.relative_to(ROOT).parts:
        parent = parent / part
        parent.mkdir(mode=0o700, exist_ok=True)
    output.mkdir(mode=0o700)
    home, work = output / "home", output / "work"
    home.mkdir(mode=0o700)
    work.mkdir(mode=0o700)
    # A cleared child environment cannot inherit a managed run or its registry,
    # broker, capabilities or identity. The native launcher uses this same map.
    env = {key: os.environ[key] for key in ("PATH", "SYSTEMROOT", "LANG") if key in os.environ}
    env.update(HOME=str(home), USERPROFILE=str(home), ORBIT_SKIP_HOST_PREREQUISITES="1")
    stub = output / "stub-bin"
    stub.mkdir(mode=0o700)
    (stub / "codex").write_text("#!/bin/sh\nexit 0\n")
    (stub / "codex").chmod(0o700)
    env["PATH"] = str(stub) + os.pathsep + env.get("PATH", "")
    transcript = []

    def run(argv):
        result = subprocess.run(argv, cwd=work, env=env, text=True, capture_output=True, timeout=120)
        transcript.append({"command": argv, "exit_code": result.returncode, "stdout": result.stdout, "stderr": result.stderr})
        if result.returncode:
            raise RuntimeError(f"command failed: {argv}: {result.stderr}")
        return result.stdout

    version = run([str(binary), "--version"]).strip()
    run(["git", "init", "--quiet"])
    run([str(binary), "init", "--non-interactive", "--skip-host-prerequisites", "--machine-name", "mcp-apps-probe", "--task-prefix", "TST"])
    run([str(binary), "workspace", "init", "--name", "mcp-apps-probe"])
    routed = json.loads(run([str(binary), "workspace", "show", "--format", "json"]))
    if routed["checkout"]["repo_root"] != str(work):
        raise RuntimeError("fixture routing mismatch; no task authored")
    lines = queue.Queue(maxsize=256)
    stderr = (output / "server-stderr.log").open("w")
    process = subprocess.Popen([str(binary), "mcp", "serve"], cwd=work, env=env,
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, text=True)
    def receive(stream):
        for line in stream:
            lines.put(line)
    threading.Thread(target=receive, args=(process.stdout,), daemon=True).start()
    counter = 0
    def rpc(method, params):
        nonlocal counter
        counter += 1
        request = {"jsonrpc": "2.0", "id": counter, "method": method, "params": params}
        process.stdin.write(json.dumps(request) + "\n")
        process.stdin.flush()
        while True:
            response = json.loads(lines.get(timeout=120))
            if response.get("id") == counter:
                transcript.append({"request": request, "response": response})
                return response
    def call(name, arguments):
        response = rpc("tools/call", {"name": name, "arguments": arguments})
        result = response.get("result")
        if not result or result.get("isError"):
            raise RuntimeError(f"tool call failed: {response}")
        require(isinstance(result.get("structuredContent"), dict), "structured tool response absent")
        return result["structuredContent"]
    def refused(name, arguments):
        response = rpc("tools/call", {"name": name, "arguments": arguments})
        data = response.get("result", {}).get("structuredContent", {})
        require("error" in response or response.get("result", {}).get("isError") is True
                or (data.get("mutation_applied") is False and data.get("refusal")),
                f"expected refusal for {name}: {response}")
    def read(scope, **fields):
        name = {"tasks": "orbit_task_list", "task": "orbit_task_show", "runs": "orbit_workflow_run_list", "run": "orbit_workflow_run_show"}[scope]
        return call(name, {"workspace": str(work), "view": "bounded", **fields})
    def write_arguments(request_id, operation):
        operation = dict(operation)
        kind = operation.pop("kind")
        if kind == "edit":
            operation.update(operation.pop("fields"))
        return ("orbit_task_add" if kind == "create" else "orbit_task_update",
                {"workspace": str(work), "model": "codex", "request_id": request_id, **operation})
    def write(request_id, operation):
        name, args = write_arguments(request_id, operation)
        return call(name, args)
    try:
        initialized = rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "orbit-apps-probe", "version": "1"}})
        if not initialized.get("result", {}).get("capabilities", {}).get("resources") == {}:
            raise RuntimeError(f"resources capability absent: {initialized}")
        process.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        process.stdin.flush()
        tools = rpc("tools/list", {})["result"]["tools"]
        by_name = {tool["name"]: tool for tool in tools}
        for name, entrypoint in (("orbit_ui_open", "global"), ("orbit_ui_inspect", "thread")):
            tool = by_name[name]
            require(tool["_meta"]["ui"]["resourceUri"] == URI, f"wrong UI URI for {name}")
            require(tool["_meta"]["openai/ui"]["entrypoints"] == [{"type": entrypoint}], "entrypoint metadata mismatch")
            require(tool["annotations"]["readOnlyHint"] is True, "opening a panel must be read-only")
        for name in ("orbit_task_show", "orbit_task_list", "orbit_task_add", "orbit_task_update", "orbit_workflow_auto", "orbit_routine_control", "orbit_pipeline_invoke"):
            require(name in by_name, f"missing daily control-center tool {name}")
            require("ui" not in by_name[name].get("_meta", {}), "ordinary data tools acquired automatic widgets")
        require(not any(name.startswith("orbit_desktop_") for name in by_name), "retired desktop wrappers are still advertised")
        resources = rpc("resources/list", {})["result"]["resources"]
        require(any(resource["uri"] == URI for resource in resources), "canonical resource not listed")
        contents = rpc("resources/read", {"uri": URI})["result"]["contents"][0]
        require(contents["mimeType"] == "text/html;profile=mcp-app", "wrong MCP Apps MIME")
        require(contents["_meta"]["ui"]["csp"]["connectDomains"] == [], "resource allows network connections")
        require(contents["_meta"]["ui"]["csp"]["resourceDomains"] == [], "resource allows external assets")
        (output / "task-panel.html").write_text(contents["text"])
        legacy = rpc("resources/read", {"uri": LEGACY_URI})["result"]["contents"][0]
        require(legacy["text"] == contents["text"], "old open panels cannot load compatible resource")
        for uri in ("file:///etc/passwd", URI + "?workspace=other", URI + "/../index.html"):
            require("error" in rpc("resources/read", {"uri": uri}), "arbitrary resource URI accepted")
        require(call("orbit_ui_open", {})["workspace"] is None, "empty open inferred a workspace")
        create = {"kind": "create", "title": "MCP Apps disposable task <script>not executable</script>",
                  "description": "Disposable daily control-center protocol fixture.",
                  "acceptance_criteria": ["The disposable protocol checks preserve task identity"], "priority": "medium"}
        created = write("probe-create", create)
        task_key = created["snapshot"]["task"]["id"]
        require(created["snapshot"]["task"]["status"] == "proposed", "create skipped proposed state")
        replay = write("probe-create", create)
        require(replay["replayed"] is True and replay["snapshot"]["task"]["id"] == task_key, "create retry duplicated a task")
        refused(*write_arguments("probe-create", {**create, "title": "conflicting retry"}))
        listed = read("tasks", search="MCP Apps disposable task", status="proposed", limit=1)
        require(listed["total"] == 1 and listed["items"][0]["id"] == task_key, "filtered task list is inconsistent")
        require(listed["search_scope"] == "task key and title", "search scope missing")
        empty = read("tasks", search="MCP Apps disposable task", offset=1, limit=1)
        require(empty["total"] == 1 and empty["items"] == [], "pagination total changed with offset")
        panel = call("orbit_ui_inspect", {"workspace": str(work), "id": task_key})
        require(panel["task"]["id"] == task_key and panel["workspace"] == str(work), "panel destination mismatch")
        require(call("orbit_task_show", {"workspace": str(work), "id": task_key})["title"] == panel["task"]["title"], "ordinary reader disagrees")
        snapshot = call("orbit_task_show", {"workspace": str(work), "id": task_key, "snapshot": True})
        stale_revision = snapshot["revision"]
        edit = {"kind": "edit", "id": task_key, "expected_revision": stale_revision,
                "fields": {"description": "Edited in disposable protocol fixture."}}
        edited = write("probe-edit", edit)["snapshot"]
        require(edited["revision"] != stale_revision, "edit did not change revision")
        conflict = write("probe-stale-edit", {**edit, "fields": {"description": "Must not overwrite fresh evidence"}})
        require(conflict.get("conflict", {}).get("code") == "revision_conflict", "stale edit did not return a typed conflict")
        require(conflict["snapshot"]["revision"] == edited["revision"], "conflict did not carry a fresh snapshot")
        current = read("task", id=task_key, limit=1)
        require(current["task"]["description"] == "Edited in disposable protocol fixture.", "stale write changed task")
        comment = {"kind": "comment", "id": task_key, "expected_revision": current["revision"], "comment": "Disposable review note"}
        commented = write("probe-comment", comment)
        retried = write("probe-comment", comment)
        require(retried["replayed"] is True, "comment retry was not reconciled")
        require(commented["snapshot"]["comments_total"] == retried["snapshot"]["comments_total"], "comment retry duplicated evidence")
        read("task", id=task_key, comments_offset=0, history_offset=1, artifacts_offset=0, limit=1)
        # Explicit fixture setup only: no dispatch, provider invocation or live task state.
        run([str(binary), "task", "update", task_key, "--status", "review", "--force",
             "--execution-summary", "Disposable protocol evidence; no implementation run was launched.", "--model", "codex", "--json"])
        current = read("task", id=task_key)
        criterion = current["task"]["acceptance_criteria"][0]
        verdict = {"decision": "changes_requested", "rationale": "Exercise evidence-bound review without completion.",
                   "criteria": [{"criterion": criterion, "met": False, "evidence": ["execution_summary"]}],
                   "evidence": ["execution_summary"], "expected_run_id": None, "expected_head": None}
        review = {"kind": "review", "id": task_key, "expected_revision": current["revision"], "verdict": verdict, "complete": False}
        reviewed = write("probe-review-changes", review)["snapshot"]
        require(reviewed["task"]["status"] == "review", "changes requested silently rejected/reopened task")
        accepted_verdict = {**verdict, "decision": "accept", "criteria": [{"criterion": criterion, "met": True, "evidence": ["execution_summary"]}]}
        accept = {**review, "expected_revision": reviewed["revision"], "verdict": accepted_verdict}
        accepted = write("probe-review-accept", accept)["snapshot"]
        require(accepted["task"]["status"] == "review", "record-only acceptance silently completed task")
        require(accepted["actions"]["complete"]["enabled"] is False, "unprivileged MCP claims completion authority")
        refused(*write_arguments("probe-complete-refused", {**accept, "expected_revision": accepted["revision"], "complete": True}))
        after = read("task", id=task_key)
        require(after["revision"] == accepted["revision"], "refused completion left a partial mutation")
        require(read("tasks", status="review")["total"] == 1, "review queue does not show fixture")
        refused("orbit_workflow_run_list", {"workspace": str(work), "view": "bounded"})
        refused("orbit_workflow_run_show", {"workspace": str(work), "view": "bounded", "id": "jrun-probe-unprivileged"})
        refused("orbit_ui_inspect", {"id": task_key})
        refused("orbit_ui_inspect", {"workspace": "missing-disposable-workspace", "id": task_key})
        forged_name, forged_args = write_arguments("probe-forged-authority", {**accept, "expected_revision": after["revision"], "complete": True})
        refused(forged_name, {**forged_args, "actor": "operator"})
        # This reference is data only. Actual updateModelContext transport is tested by
        # the shipped-script harness and must still be observed in the desktop host.
        reference = {"workspace": after["workspace"], "kind": "task", "id": task_key,
                     "revision": after["revision"], "observed_at": after["observed_at"],
                     "title": after["task"]["title"][:180], "instruction": "Reread authoritative Orbit state before acting."}
        (output / "context-reference.json").write_text(json.dumps(reference, indent=2) + "\n")
    finally:
        process.kill()
        process.wait(timeout=10)
        stderr.close()
        (output / "protocol-transcript.json").write_text(json.dumps(transcript, indent=2) + "\n")
    launcher = output / "serve-candidate.py"
    launcher.write_text("#!" + sys.executable + "\nimport os\nos.chdir(" + repr(str(work)) + ")\nos.execve(" + repr(str(binary)) + ", " + repr([str(binary), "mcp", "serve"]) + ", " + repr(env) + ")\n")
    launcher.chmod(0o700)
    plugin = work / "plugins/orbit-probe"
    (plugin / ".codex-plugin").mkdir(parents=True, mode=0o700)
    manifest = {"name": "orbit-probe", "description": "Isolated Orbit daily control-center candidate", "version": "0.0.0",
                "mcpServers": {"orbit-probe": {"command": str(launcher), "args": []}}}
    (plugin / ".codex-plugin/plugin.json").write_text(json.dumps(manifest, indent=2) + "\n")
    marketplace = work / ".agents/plugins/marketplace.json"
    marketplace.parent.mkdir(parents=True, mode=0o700)
    marketplace.write_text(json.dumps({"name": "orbit-probe-local", "plugins": [{
        "name": "orbit-probe", "source": {"source": "local", "path": "./plugins/orbit-probe"},
        "policy": {"installation": "AVAILABLE", "authentication": "ON_INSTALL"},
        "category": "Productivity"
    }]}, indent=2) + "\n")
    evidence = json.loads((ROOT / "docs/qa/mcp-apps-evidence-template.json").read_text())
    changed = subprocess.check_output(["git", "diff", "--name-only", "HEAD", "-z"], cwd=ROOT).split(b"\0")
    untracked = subprocess.check_output(["git", "ls-files", "--others", "--exclude-standard", "-z"], cwd=ROOT).split(b"\0")
    digest = hashlib.sha256()
    for relative in sorted(set(changed + untracked) - {b""}):
        path = ROOT / os.fsdecode(relative)
        digest.update(relative + b"\0")
        digest.update(path.read_bytes() if path.is_file() else b"DELETED")
    evidence["candidate_diff_sha256"] = digest.hexdigest()
    evidence.update(candidate_head=subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
                    backend={"binary": str(binary), "version": version},
                    fixture={"workspace": str(work), "task_key": task_key},
                    plugin={"source": str(plugin), "installed_path": None},
                    automated={"protocol": "PASS", "transcript": str(output / "protocol-transcript.json"),
                               "ui_behavior": "NOT RUN", "context_bridge": "NOT RUN",
                               "scenarios": ["resource_contract", "ordinary_client", "explicit_destination", "create_retry", "request_id_collision_refusal", "filtered_pagination", "revision_guarded_edit", "stale_edit_refusal", "comment_retry", "evidence_bound_review", "completion_refusal", "run_authority_refusal"]})
    evidence["native"]["reason"] = "NOT RUN/deferred: native handoff evidence is retained in archived ORB-13715; protocol checks do not establish native UI or model-context support."
    evidence["notes"].append("Fixture review state was seeded by explicit CLI --force in disposable state; no execution was dispatched.")
    evidence["notes"].append("Request receipts survive restart with retained workspace/task storage; no TTL. Desktop is not an operator-authority source.")
    (output / "evidence.json").write_text(json.dumps(evidence, indent=2) + "\n")
    print(json.dumps({"protocol": "PASS", "native": "NOT RUN", "evidence": str(output / "evidence.json"), "plugin": str(plugin), "workspace": str(work), "task_key": task_key}))


if __name__ == "__main__":
    main()

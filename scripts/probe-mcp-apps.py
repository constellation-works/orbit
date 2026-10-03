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
URI = "ui://orbit/task-panel/v1/index.html"


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
    task = json.loads(run([str(binary), "tool", "run", "orbit.task.add", "--input", json.dumps({
        "workspace": str(work), "title": "MCP Apps disposable task <script>not executable</script>",
        "description": "Read-only native probe fixture", "complexity": "low", "model": "codex"
    })]))
    task_key = task["id"]
    lines = queue.Queue(maxsize=256)
    stderr = (output / "server-stderr.log").open("w")
    process = subprocess.Popen([str(binary), "mcp", "serve"], cwd=work, env=env,
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, text=True)
    def receive():
        for line in process.stdout:
            lines.put(line)
    threading.Thread(target=receive, daemon=True).start()
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
        return result["structuredContent"]
    try:
        initialized = rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "orbit-apps-probe", "version": "1"}})
        if not initialized.get("result", {}).get("capabilities", {}).get("resources") == {}:
            raise RuntimeError(f"resources capability absent: {initialized}")
        process.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        process.stdin.flush()
        tools = rpc("tools/list", {})["result"]["tools"]
        for name, entrypoint in (("orbit_ui_open", "global"), ("orbit_ui_inspect", "thread")):
            tool = next(tool for tool in tools if tool["name"] == name)
            assert tool["_meta"]["ui"]["resourceUri"] == URI
            assert tool["_meta"]["openai/ui"]["entrypoints"] == [{"type": entrypoint}]
        resources = rpc("resources/list", {})["result"]["resources"]
        assert any(resource["uri"] == URI for resource in resources)
        contents = rpc("resources/read", {"uri": URI})["result"]["contents"][0]
        assert contents["mimeType"] == "text/html;profile=mcp-app"
        (output / "task-panel.html").write_text(contents["text"])
        assert "error" in rpc("resources/read", {"uri": "file:///etc/passwd"})
        panel = call("orbit_ui_inspect", {"workspace": str(work), "id": task_key})
        assert panel["task"]["id"] == task_key and panel["workspace"] == str(work)
        assert call("orbit_task_show", {"id": task_key})["title"] == panel["task"]["title"]
        refused = rpc("tools/call", {"name": "orbit_ui_inspect", "arguments": {"id": task_key}})
        assert refused["result"]["isError"] is True
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
    manifest = {"name": "orbit-probe", "description": "Isolated read-only Orbit MCP Apps candidate", "version": "0.0.0",
                "mcpServers": {"orbit-probe": {"command": str(launcher), "args": []}}}
    (plugin / ".codex-plugin/plugin.json").write_text(json.dumps(manifest, indent=2) + "\n")
    marketplace = work / ".agents/plugins/marketplace.json"
    marketplace.parent.mkdir(parents=True, mode=0o700)
    marketplace.write_text(json.dumps({"name": "orbit-probe-local", "plugins": [{
        "name": "orbit-probe", "source": {"source": "local", "path": "./plugins/orbit-probe"},
        "policy": {"installation": "AVAILABLE", "authentication": "ON_INSTALL"},
        "category": "Productivity"
    }]}, indent=2) + "\n")
    evidence = json.loads((ROOT / "docs/mcp-apps-evidence-template.json").read_text())
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
                               "ui_behavior": "NOT RUN"})
    (output / "evidence.json").write_text(json.dumps(evidence, indent=2) + "\n")
    print(json.dumps({"protocol": "PASS", "native": "NOT RUN", "evidence": str(output / "evidence.json"), "plugin": str(plugin), "workspace": str(work), "task_key": task_key}))


if __name__ == "__main__":
    main()

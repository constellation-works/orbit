#!/usr/bin/env python3
"""Regress the observed nested-Bubblewrap smoke failure at the script boundary.

Fixtures replace package installation and Orbit commands; Git and the script's
HTTP client still run. No provider, live Orbit state or network install is used.
"""

import json
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("wsl2-smoke.sh").resolve()
SCRATCH = SCRIPT.parent.parent / ".orbit" / "tmp"
NAMESPACE_DENIED = (
    "Bubblewrap capability probe failed: bwrap: No permissions to create a new "
    "namespace, likely because the kernel does not allow non-privileged user namespaces."
)

ORBIT_FIXTURE = r'''#!/usr/bin/env python3
import http.server, json, os, pathlib, sys
args = sys.argv[1:]
with open(os.environ["SMOKE_FIXTURE_CALLS"], "a") as log:
    log.write(json.dumps(args) + "\n")
if args[:2] == ["doctor", "providers"]:
    print(pathlib.Path(os.environ["SMOKE_FIXTURE_ROWS"]).read_text())
elif args[:2] == ["workspace", "init"]:
    pathlib.Path(".gitignore").write_text(".orbit/\n")
elif args[:2] == ["web", "serve"]:
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"smoke fixture")
        def log_message(self, *_):
            pass
    http.server.HTTPServer(("127.0.0.1", int(args[args.index("--port") + 1])), Handler).serve_forever()
elif args[:2] == ["task", "add"]:
    sys.exit("independent task failure must not become a namespace skip")
elif args == ["--version"]:
    print("orbit smoke fixture")
elif args[0] not in ("init", "doctor", "config"):
    sys.exit("unexpected fixture command: " + repr(args))
'''

NPM_FIXTURE = r'''#!/usr/bin/env python3
import os, pathlib, shutil, sys
if sys.argv[1:] == ["--version"]:
    print("10.0.0")
elif sys.argv[1:] == ["install", "-g", "@orbit-tools/cli"]:
    target = pathlib.Path(os.environ["npm_config_prefix"]) / "bin" / "orbit"
    target.parent.mkdir(parents=True)
    shutil.copy2(os.environ["SMOKE_FIXTURE_ORBIT"], target)
else:
    sys.exit("unexpected npm command")
'''


def provider(detail, ready=False):
    return {"name": "codex", "sandbox": "linux-bwrap", "sandbox_ready": ready,
            "sandbox_readiness_detail": detail}


class NestedSandboxRegression(unittest.TestCase):
    def run_smoke(self, rows):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="wsl2-regression-", dir=SCRATCH) as tmp:
            root = Path(tmp)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            for name, text in {
                "node": "#!/bin/sh\nexit 0\n",
                "uname": "#!/bin/sh\ncase $1 in -r) echo 6.8.0-fixture ;; -s) echo Linux ;; -m) echo x86_64 ;; esac\n",
                "npm": NPM_FIXTURE,
                "orbit-fixture": ORBIT_FIXTURE,
            }.items():
                path = bin_dir / name
                path.write_text(text)
                path.chmod(0o755)
            rows_path = root / "providers.json"
            rows_path.write_text(json.dumps(rows))
            calls_path = root / "calls.jsonl"
            env = {
                "PATH": f"{bin_dir}:/usr/bin:/bin", "TMPDIR": tmp, "USER": "smoke",
                # Enter with an already cleared fixture environment so the
                # package fixtures are used instead of a real npm download.
                "ORBIT_SMOKE_CLEAN_ENV": "1", "ORBIT_SMOKE_POWERSHELL": "",
                "SMOKE_FIXTURE_ROWS": str(rows_path),
                "SMOKE_FIXTURE_CALLS": str(calls_path),
                "SMOKE_FIXTURE_ORBIT": str(bin_dir / "orbit-fixture"),
            }
            completed = subprocess.run(
                ["bash", str(SCRIPT)], env=env, cwd=root, text=True,
                capture_output=True, timeout=30, check=False,
            )
            self.assertTrue(completed.stdout, completed.stderr)
            report = json.loads(completed.stdout)
            calls = [json.loads(line) for line in calls_path.read_text().splitlines()]
            self.assertFalse(list(root.glob("orbit-wsl2-smoke.*")), "smoke scratch must be cleaned")
            return completed.returncode, report, calls

    def test_namespace_denial_skips_sandbox_and_ship(self):
        for detail in (
            NAMESPACE_DENIED,
            "Bubblewrap capability probe failed: bwrap: Creating new namespace failed: Operation not permitted",
        ):
            with self.subTest(detail=detail):
                code, report, calls = self.run_smoke([provider(detail)])
                self.assertEqual(code, 0, report)
                self.assertEqual(report["status"], "pass")
                steps = {row["step"]: row for row in report["steps"]}
                for name in ("sandbox", "ship"):
                    self.assertEqual(steps[name]["status"], "skipped", report)
                    self.assertEqual(steps[name]["reason"], "nested_sandbox")
                    self.assertIn("nested or unprivileged sandbox", steps[name]["detail"])
                self.assertFalse(any(call[:2] == ["task", "add"] for call in calls))
                self.assertEqual(steps["dashboard"]["status"], "pass")

    def test_missing_or_misconfigured_bubblewrap_still_fails(self):
        cases = (
            [provider("trusted Bubblewrap not available at /usr/bin/bwrap")],
            [provider("Bubblewrap does not support the required --bind-fd and --ro-bind-fd")],
            [provider("Bubblewrap capability probe failed: bwrap: setting up uid map: Permission denied")],
            [provider("Bubblewrap capability probe failed: bwrap: Can't bind mount: Permission denied")],
            [provider(NAMESPACE_DENIED), provider("could not execute Bubblewrap: Permission denied")],
            [],
        )
        for rows in cases:
            with self.subTest(rows=rows):
                code, report, _ = self.run_smoke(rows)
                self.assertEqual(code, 1, report)
                self.assertEqual(report["status"], "fail")
                sandbox = next(row for row in report["steps"] if row["step"] == "sandbox")
                self.assertEqual(sandbox["status"], "fail", report)
                self.assertNotIn("reason", sandbox)

    def test_independent_ship_failure_is_not_skipped(self):
        code, report, calls = self.run_smoke([provider("capability probe succeeded", ready=True)])
        self.assertEqual(code, 1, report)
        self.assertEqual(report["status"], "fail")
        ship = next(row for row in report["steps"] if row["step"] == "ship")
        self.assertEqual(ship["status"], "fail", report)
        self.assertNotIn("reason", ship)
        self.assertTrue(any(call[:2] == ["task", "add"] for call in calls))


if __name__ == "__main__":
    unittest.main()

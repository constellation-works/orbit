#!/usr/bin/env python3
"""Exercise dashboard fixture setup through the real CLI and its failure boundary."""
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import unittest

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/visual-fixture-server.sh"
SEED = ROOT / "crates/orbit-web/visual/fixtures/busy-workspace.json"
SCRATCH = ROOT / ".orbit/tmp"


class VisualFixtureServerTest(unittest.TestCase):
    def test_seed_readback_and_runner_protocol(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=SCRATCH) as tmp:
            env = {**os.environ, "ORBIT_SCRATCH_DIR": tmp}
            with open(Path(tmp) / "stdout", "w+") as stdout, open(Path(tmp) / "stderr", "w+") as stderr:
                server = subprocess.Popen([str(SCRIPT), "busy-workspace"], cwd=ROOT, env=env,
                                          stdout=stdout, stderr=stderr, start_new_session=True)
                try:
                    deadline = time.monotonic() + 90
                    output = ""
                    while time.monotonic() < deadline:
                        stdout.seek(0)
                        output = stdout.read()
                        if "ORBIT_VISUAL_TASK=" in output or server.poll() is not None:
                            break
                        time.sleep(0.1)
                    stderr.seek(0)
                    diagnostics = stderr.read()
                    self.assertIn("ORBIT_VISUAL_URL=http://127.0.0.1:", output, diagnostics)
                    fixture = next(Path(tmp).glob("visual-fixture-busy-workspace-*"))
                    env["HOME"] = str(fixture / "home")
                    orbit = env.get("ORBIT_VISUAL_ORBIT", "orbit")
                    result = subprocess.run([orbit, "--root", str(fixture / "root"), "--json", "task", "list"],
                                            cwd=fixture / "work", env=env, capture_output=True, text=True, check=True)
                    actual = {task["title"]: task["status"] for task in json.loads(result.stdout)}
                    expected = {task["title"]: task["status"] for task in json.loads(SEED.read_text())["tasks"]}
                    self.assertEqual(actual, expected)
                    first_id = (fixture / "task-ids").read_text().splitlines()[0]
                    self.assertIn(f"ORBIT_VISUAL_TASK={first_id}\n", output)
                finally:
                    if server.poll() is None:
                        os.killpg(server.pid, signal.SIGTERM)
                    server.wait(timeout=15)

    def test_faketime_launch_uses_frozen_absolute_timestamp(self):
        # Launch-boundary regression for ORB-20505: the script used to run
        # `faketime -f "$scratch/faketime.rc"` and write "@<epoch>" there.
        # `-f` passes its argument directly as FAKETIME (no file is read),
        # so a pathname is rejected, and "@..." is a running clock.
        # A stub faketime first on PATH records argv/env and execs the
        # real orbit; it does not emulate libfaketime.
        SCRATCH.mkdir(parents=True, exist_ok=True)
        for frozen_env, expected in (
            (None, "2026-01-01 12:00:00"),
            ("2027-06-15T08:30:00Z", "2027-06-15 08:30:00"),
        ):
            with self.subTest(frozen=frozen_env or "default"), tempfile.TemporaryDirectory(dir=SCRATCH) as tmp:
                stub_bin = Path(tmp) / "stub-bin"
                stub_bin.mkdir()
                record = Path(tmp) / "faketime.argv"
                stub = stub_bin / "faketime"
                stub.write_text(
                    "#!/bin/sh\n"
                    "printf '%s\\n' \"$@\" > \"$FAKETIME_STUB_RECORD\"\n"
                    "printf 'FAKETIME_DONT_FAKE_MONOTONIC=%s\\nTZ=%s\\n' "
                    "\"${FAKETIME_DONT_FAKE_MONOTONIC:-}\" \"${TZ:-}\" "
                    "> \"$FAKETIME_STUB_RECORD.env\"\n"
                    "while [ $# -gt 0 ]; do\n"
                    "  case \"$1\" in\n"
                    "    -f) shift 2; break ;;\n"
                    "    --exclude-monotonic) shift ;;\n"
                    "    *) break ;;\n"
                    "  esac\n"
                    "done\n"
                    "exec \"$@\"\n"
                )
                stub.chmod(0o755)
                env = {
                    **os.environ,
                    "ORBIT_SCRATCH_DIR": tmp,
                    "FAKETIME_STUB_RECORD": str(record),
                    "PATH": str(stub_bin) + os.pathsep + os.environ.get("PATH", ""),
                }
                if frozen_env is None:
                    env.pop("ORBIT_VISUAL_FROZEN_INSTANT", None)
                else:
                    env["ORBIT_VISUAL_FROZEN_INSTANT"] = frozen_env
                with open(Path(tmp) / "stdout", "w+") as stdout, open(Path(tmp) / "stderr", "w+") as stderr:
                    server = subprocess.Popen([str(SCRIPT), "busy-workspace"], cwd=ROOT, env=env,
                                              stdout=stdout, stderr=stderr, start_new_session=True)
                    try:
                        deadline = time.monotonic() + 90
                        output = ""
                        while time.monotonic() < deadline:
                            stdout.seek(0)
                            output = stdout.read()
                            if "ORBIT_VISUAL_URL=" in output or server.poll() is not None:
                                break
                            time.sleep(0.1)
                        stderr.seek(0)
                        diagnostics = stderr.read()
                        self.assertIn("ORBIT_VISUAL_URL=http://127.0.0.1:", output, diagnostics)
                        self.assertIn("server_clock=faketime", diagnostics)
                        argv = record.read_text().splitlines()
                        self.assertGreaterEqual(len(argv), 3, argv)
                        self.assertEqual(argv[0], "-f", argv)
                        self.assertEqual(argv[1], expected, argv)
                        self.assertRegex(argv[1], r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$")
                        self.assertNotIn("/", argv[1], argv)
                        self.assertFalse(argv[1].startswith("@"), argv)
                        # Remaining argv is the real orbit serve command.
                        self.assertIn("web", argv)
                        self.assertIn("serve", argv)
                        recorded_env = (Path(str(record) + ".env")).read_text()
                        self.assertIn("FAKETIME_DONT_FAKE_MONOTONIC=1", recorded_env)
                        self.assertIn("TZ=UTC", recorded_env)
                    finally:
                        if server.poll() is None:
                            os.killpg(server.pid, signal.SIGTERM)
                        server.wait(timeout=15)

    def test_seed_failure_never_announces_readiness(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        for operation, response in (
            ("task add", 'echo "injected add failure" >&2; exit 9'),
            ("task update", 'echo "injected update failure" >&2; exit 9'),
            ("task show", "printf '%s\\n' '{\"status\":\"wrong\"}'; exit 0"),
        ):
            with self.subTest(operation=operation), tempfile.TemporaryDirectory(dir=SCRATCH) as tmp:
                wrapper = Path(tmp) / "orbit-fail"
                wrapper.write_text(f'#!/bin/sh\ncase " $* " in *" {operation} "*) {response};; esac\nexec orbit "$@"\n')
                wrapper.chmod(0o755)
                result = subprocess.run([str(SCRIPT), "busy-workspace"], cwd=ROOT,
                                        env={**os.environ, "ORBIT_SCRATCH_DIR": tmp, "ORBIT_VISUAL_ORBIT": str(wrapper)},
                                        capture_output=True, text=True, timeout=90)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("expected proposed, got wrong" if operation == "task show" else "injected", result.stderr)
                self.assertNotIn("ORBIT_VISUAL_URL=", result.stdout)

    def test_candidate_assets_ignore_stale_embedded_backend(self):
        # The previous fixture announced the backend's own URL, so a binary
        # that embeds STALE-EMBEDDED-ASSET was what the browser rendered while
        # metadata still named the checkout. The front must serve the checkout
        # change, keep backend identity separate, and answer overlays itself.
        SCRATCH.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=SCRATCH) as tmp:
            repo = self._mini_checkout(Path(tmp) / "repo")
            wrapper, record = self._stale_backend(Path(tmp), "present")
            env = self._fixture_env(tmp, wrapper, record, "present")
            server, output, diagnostics = self._start(repo / "scripts/visual-fixture-server.sh", ["experiments"], repo, env)
            try:
                self.assertIn("ORBIT_VISUAL_URL=http://127.0.0.1:", output, diagnostics)
                protocol = self._protocol(output)
                url = protocol["ORBIT_VISUAL_URL"]
                status, headers, body = self._fetch(url, "/static/dashboard.css")
                self.assertEqual(status, 200, body)
                self.assertNotIn(b"STALE-EMBEDDED-ASSET", body)
                self.assertIn(b"color: rgb(1, 2, 3)", body)
                self.assertLess(body.index(b"color: rgb(1, 2, 3)"), body.index(b"outline-color: rgb(4, 5, 6)"))
                self.assertIn("default-src 'self'", headers.get("Content-Security-Policy", ""))
                self.assertTrue(headers.get("Content-Type", "").startswith("text/css"))
                js_status, js_headers, js_body = self._fetch(url, "/static/app.js")
                self.assertEqual(js_status, 200)
                self.assertIn(b"CANDIDATE-JS-MARKER", js_body)
                self.assertNotIn(b"STALE-EMBEDDED-ASSET", js_body)
                self.assertTrue(js_headers.get("Content-Type", "").startswith("application/javascript"))
                missing_status, _, missing_body = self._fetch(url, "/static/not-declared.js")
                self.assertEqual(missing_status, 404, missing_body)
                self.assertIn(b"candidate asset missing", missing_body)
                self.assertNotIn(b"STALE-EMBEDDED-ASSET", missing_body)
                for escaped in ("/static/../scripts/visual-fixture-server.sh", "/%2e%2e/scripts/visual-fixture-server.sh"):
                    escape_status, _, escape_body = self._fetch(url, escaped)
                    self.assertEqual(escape_status, 400, escape_body)
                    self.assertNotIn(b"STALE-EMBEDDED-ASSET", escape_body)
                    self.assertNotIn(b"#!/bin/sh", escape_body)
                experiment_status, _, experiment_body = self._fetch(url, "/api/diagnostics/experiments/exp-visual-fixture")
                self.assertEqual(experiment_status, 200, experiment_body)
                self.assertEqual(json.loads(experiment_body)["id"], "exp-visual-fixture")
                scorecard_status, _, scorecard_body = self._fetch(url, "/api/agents/implementers/scorecard")
                self.assertIn(b"STALE-EMBEDDED-ASSET", scorecard_body)
                self.assertEqual(scorecard_status, 200)
                provenance = json.loads(Path(protocol["ORBIT_VISUAL_PROVENANCE"]).read_text())
                self.assertEqual(provenance["source_revision"], self._head(repo))
                self.assertFalse(provenance["static_backend_identity_compared"])
                self.assertFalse(provenance["api_compatibility"]["schema_compared"])
                self.assertTrue(provenance["api_compatibility"]["established"])
                self.assertEqual(provenance["backend"]["version"], "stub-backend 0")
                self.assertNotEqual(provenance["assets"]["combined_sha256"], provenance["backend"]["sha256"])
                self.assertEqual(provenance["renderer"]["sha256"], self._sha256(repo / "scripts/visual-fixture-front.mjs"))
                self.assertEqual(protocol["ORBIT_VISUAL_SOURCE"], provenance["source_revision"])
                self.assertEqual(protocol["ORBIT_VISUAL_ASSETS_SHA256"], provenance["assets"]["combined_sha256"])
                self.assertIn("/api/tasks", provenance["api_compatibility"]["paths"])
                stale_log = record.read_text()
                self.assertNotIn("GET /static", stale_log)
                self.assertNotIn("GET / ", stale_log)
                style = self._browser(url, Path(tmp) / "shots", title_color="rgb(1, 2, 3)")
                self.assertEqual(style["color"], "rgb(1, 2, 3)")
                self.assertNotEqual(style["stale"], "stale")
                self.assertEqual(style["marker"], "CANDIDATE-JS-MARKER")
                self._copy_shots(Path(tmp) / "shots", "fixture-candidate")
            finally:
                self._stop(server)
            self._assert_port_closed(url)

    def test_missing_or_unreadable_candidate_asset_refuses_readiness(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        for case in ("missing", "unreadable"):
            with self.subTest(case=case), tempfile.TemporaryDirectory(dir=SCRATCH) as tmp:
                repo = self._mini_checkout(Path(tmp) / "repo", asset_case=case)
                result = subprocess.run(
                    [str(repo / "scripts/visual-fixture-server.sh"), "experiments"],
                    cwd=repo,
                    env={**os.environ, "ORBIT_SCRATCH_DIR": tmp, "ORBIT_FIXTURE_REPO": str(repo)},
                    capture_output=True,
                    text=True,
                    timeout=30,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("candidate asset", result.stderr)
                self.assertNotIn("ORBIT_VISUAL_URL=", result.stdout)
                if case == "unreadable":
                    os.chmod(repo / "crates/orbit-web/assets/dashboard/css/app.css", 0o644)

    def test_absent_api_route_refuses_readiness(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=SCRATCH) as tmp:
            repo = self._mini_checkout(Path(tmp) / "repo")
            wrapper, record = self._stale_backend(Path(tmp), "absent")
            env = self._fixture_env(tmp, wrapper, record, "absent")
            result = subprocess.run(
                [str(repo / "scripts/visual-fixture-server.sh"), "experiments:empty"],
                cwd=repo,
                env=env,
                capture_output=True,
                text=True,
                timeout=90,
            )
            self.assertNotEqual(result.returncode, 0, result.stderr)
            self.assertIn("source/API compatibility not established", result.stderr)
            self.assertNotIn("ORBIT_VISUAL_URL=", result.stdout)

    def test_proxied_requests_stay_on_validated_upstream_origin(self):
        # AC2: binding the front to 127.0.0.1 does not confine its outbound
        # proxy. A network-path or absolute-form request target used to
        # resolve to another origin. Two private loopback servers prove the
        # repaired front refuses those targets before dispatch and still
        # forwards an ordinary origin-form API path and query.
        SCRATCH.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=SCRATCH) as tmp:
            tmp_path = Path(tmp)
            repo = self._mini_checkout(tmp_path / "repo")
            wrapper, record = self._stale_backend(tmp_path, "present")
            decoy_script = tmp_path / "decoy.mjs"
            decoy_record = tmp_path / "decoy.record"
            decoy_script.write_text(
                "import http from 'node:http';\n"
                "import fs from 'node:fs';\n"
                "const record = process.env.DECOY_RECORD;\n"
                "fs.writeFileSync(record, '');\n"
                "http.createServer((req, res) => {\n"
                "  fs.appendFileSync(record, req.method + ' ' + req.url + '\\n');\n"
                "  res.writeHead(200, {'content-type': 'text/plain'});\n"
                "  res.end('DECOY-REACHED');\n"
                "}).listen(0, '127.0.0.1', function () {\n"
                "  console.log('http://127.0.0.1:' + this.address().port);\n"
                "});\n"
            )
            decoy = subprocess.Popen(
                ["node", str(decoy_script)],
                cwd=tmp_path,
                env={**os.environ, "DECOY_RECORD": str(decoy_record)},
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                start_new_session=True,
            )
            try:
                decoy_line = decoy.stdout.readline().strip()
                self.assertTrue(decoy_line.startswith("http://127.0.0.1:"), decoy_line)
                decoy_port = decoy_line.rsplit(":", 1)[1]
                env = self._fixture_env(tmp, wrapper, record, "present")
                server, output, diagnostics = self._start(
                    repo / "scripts/visual-fixture-server.sh", ["experiments"], repo, env,
                )
                try:
                    self.assertIn("ORBIT_VISUAL_URL=http://127.0.0.1:", output, diagnostics)
                    url = self._protocol(output)["ORBIT_VISUAL_URL"]
                    attacks = [
                        f"//127.0.0.1:{decoy_port}/marker-net",
                        f"http://127.0.0.1:{decoy_port}/marker-abs",
                        f"https://127.0.0.1:{decoy_port}/marker-tls",
                        f"///127.0.0.1:{decoy_port}/marker-slash",
                        "/" + "\\" + f"127.0.0.1:{decoy_port}/marker-bs",
                    ]
                    for target in attacks:
                        status, body = self._raw_request(url, target)
                        self.assertEqual(status, 400, (target, status, body))
                        self.assertIn(b"not confined", body)
                        self.assertNotIn(b"DECOY-REACHED", body)
                        self.assertNotIn(b"STALE-EMBEDDED-ASSET", body)
                    kept_status, _, kept_body = self._fetch(url, "/api/tasks?probe=kept")
                    self.assertEqual(kept_status, 200, kept_body)
                    self.assertIn(b"STALE-EMBEDDED-ASSET", kept_body)
                    self.assertNotIn(b"DECOY-REACHED", kept_body)
                    query_target = f"/api/tasks?next=//127.0.0.1:{decoy_port}/marker-query"
                    query_status, _, query_body = self._fetch(url, query_target)
                    self.assertEqual(query_status, 200, query_body)
                    self.assertIn(b"STALE-EMBEDDED-ASSET", query_body)
                    self.assertNotIn(b"DECOY-REACHED", query_body)
                    overlay_status, _, overlay_body = self._fetch(
                        url, "/api/diagnostics/experiments/exp-visual-fixture",
                    )
                    self.assertEqual(overlay_status, 200, overlay_body)
                    self.assertEqual(json.loads(overlay_body)["id"], "exp-visual-fixture")
                    self.assertNotIn(b"DECOY-REACHED", overlay_body)
                    decoy_hits = decoy_record.read_text()
                    self.assertEqual(decoy_hits, "", decoy_hits)
                    intended = record.read_text()
                    self.assertIn("GET /api/tasks?probe=kept\n", intended)
                    self.assertIn(f"GET {query_target}\n", intended)
                    for marker in ("marker-net", "marker-abs", "marker-tls", "marker-slash", "marker-bs"):
                        self.assertNotIn(marker, intended, intended)
                        self.assertNotIn(marker, decoy_hits)
                finally:
                    self._stop(server)
            finally:
                if decoy.poll() is None:
                    os.killpg(decoy.pid, signal.SIGTERM)
                decoy.wait(timeout=15)
                decoy.stdout.close()
                decoy.stderr.close()

    def test_admitted_checkout_assets_with_separate_backend_identity(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=SCRATCH) as tmp:
            wrapper, record = self._stale_backend(Path(tmp), "present")
            env = self._fixture_env(tmp, wrapper, record, "present")
            server, output, diagnostics = self._start(SCRIPT, ["implementers-scorecard"], ROOT, env)
            try:
                self.assertIn("ORBIT_VISUAL_URL=http://127.0.0.1:", output, diagnostics)
                protocol = self._protocol(output)
                url = protocol["ORBIT_VISUAL_URL"]
                status, headers, body = self._fetch(url, "/static/dashboard.css")
                self.assertEqual(status, 200, diagnostics)
                self.assertEqual(body, self._declared_bytes("/static/dashboard.css"))
                self.assertNotIn(b"STALE-EMBEDDED-ASSET", body)
                self.assertIn("default-src 'self'", headers.get("Content-Security-Policy", ""))
                font_status, font_headers, font_body = self._fetch(url, "/static/fonts/geist-latin.woff2")
                self.assertEqual(font_status, 200)
                self.assertEqual(font_body, (ROOT / "crates/orbit-web/assets/dashboard/fonts/geist-latin.woff2").read_bytes())
                self.assertEqual(font_headers.get("Content-Type"), "font/woff2")
                card_status, _, card_body = self._fetch(url, "/api/agents/implementers/scorecard?window=last500")
                self.assertEqual(card_status, 200, card_body[:200])
                card = json.loads(card_body)
                self.assertEqual(card["status"], "ok")
                self.assertEqual(card["window"], "last500")
                self.assertNotIn(b"STALE-EMBEDDED-ASSET", card_body)
                provenance = json.loads(Path(protocol["ORBIT_VISUAL_PROVENANCE"]).read_text())
                self.assertEqual(provenance["source_revision"], self._head(ROOT))
                # The field records the admitted checkout, including a dirty
                # worktree. A clean tree is not required for the identity split.
                porcelain = subprocess.run(
                    ["git", "-C", str(ROOT), "status", "--porcelain"],
                    check=True, capture_output=True, text=True,
                    env={**os.environ, "GIT_OPTIONAL_LOCKS": "0"},
                ).stdout
                self.assertEqual(provenance["source_dirty"], bool(porcelain.strip()))
                self.assertFalse(provenance["static_backend_identity_compared"])
                self.assertFalse(provenance["api_compatibility"]["schema_compared"])
                self.assertGreater(provenance["api_compatibility"]["probed"], 20)
                self.assertIn("/api/hosts", provenance["api_compatibility"]["paths"])
                self.assertEqual(provenance["backend"]["version"], "stub-backend 0")
                self.assertNotEqual(provenance["backend"]["sha256"], provenance["assets"]["combined_sha256"])
                self.assertNotIn("GET /static", record.read_text())
                style = self._browser(url, Path(tmp) / "shots", css_var="#0b0d10")
                self.assertEqual(style["bg"], "#0b0d10")
                self.assertNotEqual(style["stale"], "stale")
                self._copy_shots(Path(tmp) / "shots", "fixture-checkout")
                evidence = SCRATCH / "evidence"
                evidence.mkdir(parents=True, exist_ok=True)
                (evidence / "fixture-provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
            finally:
                self._stop(server)

    def _mini_checkout(self, repo, asset_case="present"):
        script_dir = repo / "scripts"
        helper_dir = repo / "crates/orbit-web/src/tests"
        asset_dir = repo / "crates/orbit-web/assets/dashboard"
        css_dir = asset_dir / "css"
        fixture_dir = repo / "crates/orbit-web/visual/fixtures"
        for directory in (script_dir, helper_dir, css_dir, fixture_dir, repo / "crates/orbit-web/src"):
            directory.mkdir(parents=True, exist_ok=True)
        shutil.copy(SCRIPT, script_dir / "visual-fixture-server.sh")
        shutil.copy(ROOT / "scripts/visual-fixture-front.mjs", script_dir / "visual-fixture-front.mjs")
        shutil.copy(ROOT / "crates/orbit-web/src/tests/dashboard_static.mjs", helper_dir / "dashboard_static.mjs")
        (script_dir / "visual-fixture-server.sh").chmod(0o755)
        (repo / "crates/orbit-web/src/lib.rs").write_text(
            'pub(crate) const DASHBOARD_CSS: &str = concat!(\n'
            '    include_str!("../assets/dashboard/css/base.css"),\n'
            '    include_str!("../assets/dashboard/css/app.css"),\n'
            ');\n'
        )
        (repo / "crates/orbit-web/src/assets.rs").write_text(
            'pub(super) const DASHBOARD_CSP: &str = concat!(\n'
            '    "default-src \'self\'; ",\n'
            '    "script-src \'self\'"\n'
            ');\n'
            'pub(super) const DASHBOARD_FILES: &[(&str, &str, &[u8])] = &[\n'
            '    ("/", HTML, include_bytes!("../assets/dashboard/index.html")),\n'
            '    ("/static/dashboard.css", CSS, DASHBOARD_CSS.as_bytes()),\n'
            '    ("/static/app.js", JS, include_bytes!("../assets/dashboard/app.js")),\n'
            '];\n'
        )
        (asset_dir / "index.html").write_text(
            '<!doctype html><html><head><link rel="stylesheet" href="/static/dashboard.css"></head>'
            '<body><h1 id="title" class="title">Candidate</h1><script src="/static/app.js"></script></body></html>\n'
        )
        (css_dir / "base.css").write_text(".title { color: rgb(1, 2, 3); }\n")
        app_css = css_dir / "app.css"
        if asset_case != "missing":
            app_css.write_text(".title { outline-color: rgb(4, 5, 6); }\n")
        (asset_dir / "app.js").write_text(
            'fetch("/api/tasks");\n'
            'globalThis.CANDIDATE_JS = "CANDIDATE-JS-MARKER";\n'
        )
        (fixture_dir / "experiments.json").write_text(json.dumps({
            "tasks": [{"title": "Fixture experiment task", "complexity": "low", "status": "proposed"}],
            "experiment": {"id": "exp-visual-fixture", "name": "Fixture"},
            "readout": {"verdict": {"state": "collecting"}},
        }))
        subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
        commit = ["git", "-c", "user.email=fixture@example.com", "-c", "user.name=fixture"]
        subprocess.run([*commit, "add", "."], cwd=repo, check=True)
        subprocess.run([*commit, "commit", "-q", "-m", "fixture"], cwd=repo, check=True)
        if asset_case == "unreadable":
            app_css.chmod(0)
        return repo

    def _stale_backend(self, directory, mode):
        record = Path(directory) / "stale.record"
        server = Path(directory) / "stale-server.mjs"
        server.write_text(
            "import http from 'node:http';\n"
            "import fs from 'node:fs';\n"
            "const record = process.env.STALE_RECORD;\n"
            "const mode = process.env.STALE_MODE || 'present';\n"
            "fs.writeFileSync(record, '');\n"
            "http.createServer((req, res) => {\n"
            "  fs.appendFileSync(record, req.method + ' ' + req.url + '\\n');\n"
            "  if (req.method === 'OPTIONS') {\n"
            "    const status = mode === 'absent' ? 404 : 405;\n"
            "    res.writeHead(status, {'content-type': 'text/plain', 'allow': 'GET'});\n"
            "    res.end(status === 404 ? 'missing' : 'method');\n"
            "    return;\n"
            "  }\n"
            "  res.writeHead(200, {'content-type': 'text/css'});\n"
            "  res.end('STALE-EMBEDDED-ASSET\\n:root{--stale-marker:stale}\\n');\n"
            "}).listen(0, '127.0.0.1', function () {\n"
            "  console.log('Dashboard listening on http://127.0.0.1:' + this.address().port);\n"
            "});\n"
        )
        real = shutil.which("orbit")
        self.assertIsNotNone(real, "orbit on PATH is the CLI half of the disposable backend")
        wrapper = Path(directory) / "orbit-stub"
        wrapper.write_text(
            "#!/bin/sh\n"
            "if [ \"$1\" = \"--version\" ]; then\n"
            "  printf '%s\\n' 'stub-backend 0'\n"
            "  exit 0\n"
            "fi\n"
            "prev=\n"
            "for arg in \"$@\"; do\n"
            "  if [ \"$prev\" = \"web\" ] && [ \"$arg\" = \"serve\" ]; then\n"
            f"    exec node {shlex.quote(str(server))}\n"
            "  fi\n"
            "  prev=$arg\n"
            "done\n"
            f"exec {shlex.quote(real)} \"$@\"\n"
        )
        wrapper.chmod(0o755)
        return wrapper, record

    def _fixture_env(self, tmp, wrapper, record, mode):
        return {
            **os.environ,
            "ORBIT_SCRATCH_DIR": tmp,
            "ORBIT_VISUAL_ORBIT": str(wrapper),
            "STALE_RECORD": str(record),
            "STALE_MODE": mode,
        }

    def _start(self, script, args, cwd, env):
        stdout_path = Path(env["ORBIT_SCRATCH_DIR"]) / "stdout"
        stderr_path = Path(env["ORBIT_SCRATCH_DIR"]) / "stderr"
        stdout = open(stdout_path, "w+")
        stderr = open(stderr_path, "w+")
        server = subprocess.Popen([str(script), *args], cwd=cwd, env=env, stdout=stdout, stderr=stderr, start_new_session=True)
        deadline = time.monotonic() + 90
        output = ""
        while time.monotonic() < deadline:
            stdout.seek(0)
            output = stdout.read()
            if "ORBIT_VISUAL_URL=" in output or server.poll() is not None:
                break
            time.sleep(0.1)
        stderr.seek(0)
        diagnostics = stderr.read()
        stdout.close()
        stderr.close()
        return server, output, diagnostics

    def _stop(self, server):
        if server.poll() is None:
            os.killpg(server.pid, signal.SIGTERM)
        server.wait(timeout=15)

    def _protocol(self, output):
        values = {}
        for line in output.splitlines():
            if line.startswith("ORBIT_VISUAL_") and "=" in line:
                key, value = line.split("=", 1)
                values[key] = value
        return values

    def _raw_request(self, url, request_target):
        parsed = urllib.parse.urlsplit(url)
        payload = (
            f"GET {request_target} HTTP/1.1\r\n"
            f"Host: {parsed.hostname}:{parsed.port}\r\n"
            "Connection: close\r\n"
            "\r\n"
        ).encode("ascii")
        with socket.create_connection((parsed.hostname, parsed.port), timeout=5) as sock:
            sock.sendall(payload)
            chunks = []
            while True:
                data = sock.recv(4096)
                if not data:
                    break
                chunks.append(data)
        raw = b"".join(chunks)
        head, _, body = raw.partition(b"\r\n\r\n")
        line = head.split(b"\r\n", 1)[0]
        self.assertTrue(line.startswith(b"HTTP/1.1 "), raw[:300])
        return int(line.split()[1]), body

    def _fetch(self, url, route):
        request = urllib.request.Request(url + route, method="GET")
        try:
            with urllib.request.urlopen(request, timeout=10) as response:
                return response.status, response.headers, response.read()
        except urllib.error.HTTPError as error:
            try:
                return error.code, error.headers, error.read()
            finally:
                error.close()

    def _head(self, repo):
        return subprocess.run(
            ["git", "-C", str(repo), "rev-parse", "HEAD"],
            check=True, capture_output=True, text=True,
            env={**os.environ, "GIT_OPTIONAL_LOCKS": "0"},
        ).stdout.strip()

    def _sha256(self, path):
        return hashlib.sha256(path.read_bytes()).hexdigest()

    def _declared_bytes(self, pathname):
        result = subprocess.run(
            ["node", "--input-type=module", "-e",
             "import { dashboardDeclaredAsset } from './crates/orbit-web/src/tests/dashboard_static.mjs';"
             "process.stdout.write(dashboardDeclaredAsset(process.argv[1]).data);",
             pathname],
            cwd=ROOT, check=True, capture_output=True,
        )
        return result.stdout

    def _browser(self, url, shot_dir, title_color="", css_var=""):
        shot_dir.mkdir(parents=True, exist_ok=True)
        module = os.environ.get("PLAYWRIGHT_MODULE")
        self.assertTrue(module, "PLAYWRIGHT_MODULE is required for fixture browser evidence")
        script = Path(shot_dir) / "check.mjs"
        script.write_text(
            "import { pathToFileURL } from 'node:url';\n"
            "const { chromium } = await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE).href);\n"
            "const url = process.argv[2];\n"
            "const shotDir = process.argv[3];\n"
            "const browser = await chromium.launch();\n"
            "const measure = async (width, name) => {\n"
            "  const page = await browser.newPage({ viewport: { width, height: width === 375 ? 812 : 900 }, deviceScaleFactor: 1, colorScheme: 'dark', reducedMotion: 'reduce' });\n"
            "  await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 20000 });\n"
            "  const style = await page.evaluate(() => {\n"
            "    const title = document.querySelector('#title');\n"
            "    return {\n"
            "      color: title ? getComputedStyle(title).color : '',\n"
            "      bg: getComputedStyle(document.documentElement).getPropertyValue('--bg').trim(),\n"
            "      stale: getComputedStyle(document.documentElement).getPropertyValue('--stale-marker').trim(),\n"
            "      marker: globalThis.CANDIDATE_JS || '',\n"
            "    };\n"
            "  });\n"
            "  await page.screenshot({ path: shotDir + '/' + name + '.jpg', type: 'jpeg', quality: 60 });\n"
            "  await page.close();\n"
            "  return style;\n"
            "};\n"
            "const desktop = await measure(1440, 'dark-1440');\n"
            "const phone = await measure(375, 'dark-375');\n"
            "await browser.close();\n"
            "if (desktop.color !== phone.color || desktop.bg !== phone.bg || desktop.marker !== phone.marker) {\n"
            "  console.error('viewport style diverged');\n"
            "  process.exit(2);\n"
            "}\n"
            "process.stdout.write(JSON.stringify(desktop));\n"
        )
        result = subprocess.run(
            ["node", str(script), url, str(shot_dir)],
            check=False, capture_output=True, text=True,
            env=os.environ,
            timeout=60,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        style = json.loads(result.stdout)
        if title_color:
            self.assertEqual(style["color"], title_color)
        if css_var:
            self.assertEqual(style["bg"], css_var)
        return style

    def _copy_shots(self, shot_dir, stem):
        evidence = SCRATCH / "evidence"
        evidence.mkdir(parents=True, exist_ok=True)
        for name in ("dark-1440.jpg", "dark-375.jpg"):
            target = evidence / f"{stem}-{name}"
            shutil.copy(shot_dir / name, target)

    def _assert_port_closed(self, url):
        deadline = time.monotonic() + 5
        closed = False
        while time.monotonic() < deadline:
            try:
                urllib.request.urlopen(url, timeout=1)
            except Exception:
                closed = True
                break
            time.sleep(0.1)
        self.assertTrue(closed, url)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Exercise local CodeQL's subprocess boundary without CodeQL or downloads."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


REPO_ROOT = Path(__file__).resolve().parent.parent
SCRATCH = Path(os.environ.get("ORBIT_SCRATCH_DIR", REPO_ROOT / ".orbit/tmp"))

STUB = '''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path

name = Path(sys.argv[0]).name
args = sys.argv[1:]
keys = ("RUSTUP_HOME", "CARGO_HOME", "CARGO_TARGET_DIR", "TMPDIR",
        "XDG_CACHE_HOME", "RUSTUP_TOOLCHAIN", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER")
with open(os.environ["STUB_CALLS"], "a") as log:
    log.write(json.dumps(dict(name=name, args=args, env={k: os.environ[k] for k in keys})) + "\\n")
if name == "rustup":
    if os.environ.get("STUB_INSTALL_FAIL"):
        print("error: could not create temp file: Read-only file system (os error 30)", file=sys.stderr)
        sys.exit(30)
    (Path(os.environ["RUSTUP_HOME"]) / "installed").write_text(args[2])
elif args[:2] == ["database", "create"]:
    database = Path(args[2])
    (database / "log").mkdir(parents=True)
    log = os.environ.get("STUB_EXTRACTION_LOG", "INFO semantic crate graph extracted")
    if os.environ.get("STUB_LOG_FILE_ONLY"):
        (database / "log/extractor.log").write_text(log)
    else:
        print(log)
    sys.exit(int(os.environ.get("STUB_CREATE_EXIT", "0")))
elif args[:2] == ["database", "analyze"]:
    if os.environ.get("STUB_ANALYZE_FAIL"):
        print("query evaluation: heap exhausted", file=sys.stderr)
        sys.exit(7)
    output = next(arg.split("=", 1)[1] for arg in args if arg.startswith("--output="))
    Path(output).write_text(json.dumps({"version": "2.1.0", "runs": [{"results": []}]}))
else:
    sys.exit(99)
'''


class LocalCodeqlTests(unittest.TestCase):
    def setUp(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="codeql-local-test.", dir=SCRATCH)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.repo = self.root / "checkout with spaces"
        scripts = self.repo / "scripts"
        scripts.mkdir(parents=True)
        self.script = scripts / "codeql-rust-local.sh"
        shutil.copy2(REPO_ROOT / "scripts/codeql-rust-local.sh", self.script)
        config = self.repo / ".github/codeql/codeql-config.yml"
        config.parent.mkdir(parents=True)
        config.write_text("name: fixture\n")
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name in ("codeql", "rustup"):
            path = self.bin / name
            path.write_text(STUB)
            path.chmod(0o755)
        self.calls_file = self.root / "calls.jsonl"
        self.global_dirs = [self.root / "user-rustup", self.root / "user-cargo"]
        for directory in self.global_dirs:
            directory.mkdir()
            (directory / "sentinel").write_text("preserve")
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                        STUB_CALLS=str(self.calls_file),
                        ORBIT_SCRATCH_DIR=str(self.root / "run scratch"),
                        RUSTUP_HOME=str(self.global_dirs[0]),
                        CARGO_HOME=str(self.global_dirs[1]))

    def run_script(self, *args, **env):
        return subprocess.run(["bash", str(self.script), *args], cwd=self.root,
                              env=dict(self.env, **env), text=True, capture_output=True,
                              timeout=20)

    def calls(self):
        return [json.loads(line) for line in self.calls_file.read_text().splitlines()]

    def assert_no_result(self, result, expected_calls):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual([call["name"] for call in self.calls()], expected_calls)
        self.assertEqual(list(self.root.rglob("results.sarif")), [])
        self.assertEqual(result.stdout, "")
        self.assert_isolated()

    def assert_isolated(self):
        for directory in self.global_dirs:
            self.assertEqual(list(directory.iterdir()), [directory / "sentinel"])
            self.assertEqual((directory / "sentinel").read_text(), "preserve")
        for call in self.calls():
            env = call["env"]
            run_dir = Path(env["RUSTUP_HOME"]).parent
            self.assertTrue(run_dir.is_relative_to(self.root / "run scratch"))
            for key in ("RUSTUP_HOME", "CARGO_HOME", "CARGO_TARGET_DIR", "TMPDIR", "XDG_CACHE_HOME"):
                self.assertTrue(Path(env[key]).is_relative_to(run_dir), (key, env[key]))
            self.assertEqual(env["RUSTC_WRAPPER"], "")
            self.assertEqual(env["RUSTC_WORKSPACE_WRAPPER"], "")
            if call["name"] == "codeql":
                for prefix in ("--common-caches=", "--logdir=", "--output=", "--compilation-cache="):
                    for arg in call["args"]:
                        if arg.startswith(prefix):
                            self.assertTrue(Path(arg[len(prefix):]).is_relative_to(run_dir))

    def test_prepares_toolchain_then_extracts_and_analyzes_named_query(self):
        query = "queries with spaces/check.ql"
        result = self.run_script("--ram", "8192", "--toolchain", "1.98.1", query)
        self.assertEqual(result.returncode, 0, result.stderr)
        install, create, analyze = self.calls()
        self.assertEqual(install["args"], ["toolchain", "install", "1.98.1", "--profile",
                                           "minimal", "--component", "rust-src", "--no-self-update"])
        self.assertEqual(install["env"]["RUSTUP_TOOLCHAIN"], "1.98.1")
        self.assertEqual(create["args"][:2], ["database", "create"])
        self.assertIn(f"--source-root={self.repo}", create["args"])
        self.assertIn("--build-mode=none", create["args"])
        self.assertIn(f"--codescanning-config={self.repo}/.github/codeql/codeql-config.yml", create["args"])
        self.assertEqual(analyze["args"][:4], ["database", "analyze", create["args"][2], query])
        self.assertIn("--ram=8192", create["args"])
        self.assertIn("--ram=8192", analyze["args"])
        self.assertIn("--no-default-compilation-cache", analyze["args"])
        self.assertEqual(len(list(self.root.rglob("results.sarif"))), 1)
        self.assert_isolated()

    def test_incomplete_extraction_never_queries_even_when_create_exits_zero(self):
        cases = [
            ("WARN semantic analyzer unavailable (unable to load manifest): macro expansion will be skipped; "
             "Rust 1.99.0: Read-only file system (os error 30)", False),
            ("macro expansion will be skipped", True),
            ("semantic analysis disabled", False),
            ("[WARN] unable to load crate graph", True),
            ("ERROR: extractor failed to resolve dependencies", False),
        ]
        for log, file_only in cases:
            with self.subTest(log=log, file_only=file_only):
                self.calls_file.write_text("")
                result = self.run_script("fixture.qls", STUB_EXTRACTION_LOG=log,
                                         STUB_LOG_FILE_ONLY="1" if file_only else "")
                self.assert_no_result(result, ["rustup", "codeql"])
                self.assertIn(log, result.stderr)
                self.assertIn(self.calls()[0]["args"][2], result.stderr)

    def test_install_refusal_names_version_and_leaves_user_homes_untouched(self):
        before = {path.relative_to(self.root) for path in self.root.rglob("*")}
        result = self.run_script("--toolchain", "1.98.1", "fixture.ql", STUB_INSTALL_FAIL="1")
        self.assert_no_result(result, ["rustup"])
        self.assertIn("1.98.1", result.stderr)
        self.assertIn("Read-only file system (os error 30)", result.stderr)
        added = {path.relative_to(self.root) for path in self.root.rglob("*")} - before
        self.assertTrue(all(path == Path("calls.jsonl") or path.parts[0] == "run scratch"
                            for path in added), added)

    def test_nonzero_extraction_without_warning_never_queries(self):
        result = self.run_script("fixture.ql", STUB_CREATE_EXIT="12")
        self.assert_no_result(result, ["rustup", "codeql"])
        self.assertIn("12", result.stderr)

    def test_failed_query_is_not_reported_as_completed(self):
        result = self.run_script("fixture.ql", STUB_ANALYZE_FAIL="1")
        self.assert_no_result(result, ["rustup", "codeql", "codeql"])
        self.assertIn("heap exhausted", result.stderr)

    def test_default_scratch_is_checkout_local(self):
        self.env.pop("ORBIT_SCRATCH_DIR")
        result = self.run_script("fixture.qls")
        self.assertEqual(result.returncode, 0, result.stderr)
        for call in self.calls():
            self.assertTrue(Path(call["env"]["RUSTUP_HOME"]).is_relative_to(self.repo / ".orbit/tmp"))


if __name__ == "__main__":
    unittest.main()

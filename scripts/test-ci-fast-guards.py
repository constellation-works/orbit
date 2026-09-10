#!/usr/bin/env python3
"""Exercise guardrail routing and dependency policy with non-compiling fixtures."""

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent


class GuardrailTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.scripts = self.root / "scripts"
        self.scripts.mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.log = self.root / "cargo.log"
        self.metadata = self.root / "metadata.json"
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                        GUARD_TEST_LOG=str(self.log), GUARD_TEST_METADATA=str(self.metadata))
        self.write_executable(self.bin / "cargo", '''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(sys.argv[1:]) + "\\n")
if sys.argv[1] == "metadata":
    print(open(os.environ["GUARD_TEST_METADATA"]).read())
elif sys.argv[1] == "test":
    print("fixture_test: test")
''')
        self.write_executable(self.bin / "rg", "#!/bin/bash\nexit 1\n")

    def write_executable(self, path, content):
        path.write_text(content)
        path.chmod(0o755)

    def run_guard(self, name, *arguments):
        shutil.copy2(SCRIPTS / name, self.scripts / name)
        return subprocess.run(["/bin/bash", str(self.scripts / name), *arguments],
                              env=self.env, text=True, capture_output=True)

    def prepare_ci(self):
        source = (SCRIPTS / "ci-guardrails.sh").read_text()
        for name in re.findall(r'\$repo_root/scripts/([\w.-]+)', source):
            self.write_executable(self.scripts / name, "#!/bin/bash\nexit 0\n")
        shutil.copy2(SCRIPTS / "check-ci-macos.sh", self.scripts / "check-ci-macos.sh")
        workflows = self.root / ".github/workflows"
        workflows.mkdir(parents=True)
        (self.root / "Cargo.toml").touch()
        (workflows / "ci-macos.yml").write_text('''on:
  pull_request:
    paths:
      - Cargo.toml
jobs:
  test:
    steps:
      - run: cargo test -p orbit-types fixture_test
''')

    def test_fast_does_not_enumerate_or_compile_tests(self):
        self.prepare_ci()
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(calls, [["fmt", "--all", "--", "--check"]])

    def test_fast_invokes_codeql_extension_schema_check(self):
        self.prepare_ci()
        self.write_executable(
            self.scripts / "check-codeql-extension-schema.py",
            '''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(["check-codeql-extension-schema.py"] + sys.argv[1:]) + "\\n")
''',
        )
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn(["check-codeql-extension-schema.py"], calls)

    def test_full_still_checks_workflow_test_matches(self):
        self.prepare_ci()
        result = self.run_guard("ci-guardrails.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn(["test", "-p", "orbit-types", "--locked", "fixture_test", "--", "--list"], calls)

    def test_full_invokes_cargo_deny_guard(self):
        self.prepare_ci()
        self.write_executable(
            self.scripts / "cargo-deny.sh",
            '''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(["cargo-deny.sh"] + sys.argv[1:]) + "\\n")
''',
        )
        result = self.run_guard("ci-guardrails.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn(["cargo-deny.sh", "check"], calls)

    def dependency_result(self, owner, dependency, kind=None):
        manifests = {}
        for name in {owner, dependency}:
            path = self.root / f"{name}.toml"
            path.touch()
            manifests[name] = str(path)
        packages = [dict(id=name, name=name, manifest_path=manifest,
                         dependencies=([dict(name=dependency, kind=kind)] if name == owner else []))
                    for name, manifest in manifests.items()]
        self.metadata.write_text(json.dumps(dict(workspace_members=list(manifests), packages=packages)))
        return self.run_guard("check-dependency-direction.sh")

    def test_allowed_dependency_on_native_bash(self):
        result = self.dependency_result("orbit-common", "orbit-types")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("dependency direction guard passed", result.stdout)

    def test_forbidden_dependency_names_owning_manifest(self):
        result = self.dependency_result("orbit-types", "orbit-common")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"forbidden dependency 'orbit-common' found in {self.root}/orbit-types.toml", result.stdout)

    def test_dev_only_dependency_rejects_production_edge(self):
        result = self.dependency_result("orbit-core", "orbit-exec")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must remain dev-only", result.stdout)

    def test_dev_only_dependency_accepts_test_edge(self):
        result = self.dependency_result("orbit-core", "orbit-exec", "dev")
        self.assertEqual(result.returncode, 0, result.stderr)


class CargoDenyGuardrailTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.log = self.root / "cargo_deny.log"
        self.fake_deny = self.bin / "cargo-deny"
        self.write_fake_deny()
        self.env = dict(
            os.environ,
            PATH=f"{self.bin}:{os.environ['PATH']}",
            FAKE_DENY_LOG=str(self.log),
            FAKE_DENY_EXIT="0",
        )
        for key in (
            "CARGO_DENY_DB_PATH",
            "ORBIT_CARGO_DENY_DB_PATH",
            "CARGO_DENY_DISABLE_FETCH",
            "ORBIT_CARGO_DENY_DISABLE_FETCH",
            "CARGO_DENY_OFFLINE",
            "ORBIT_CARGO_DENY_OFFLINE",
        ):
            self.env.pop(key, None)

    def write_fake_deny(self):
        self.fake_deny.write_text('''#!/usr/bin/env python3
import json, os, sys
log = os.environ.get("FAKE_DENY_LOG")
config_content = None
config_path = None
if "--config" in sys.argv:
    idx = sys.argv.index("--config") + 1
    if idx < len(sys.argv):
        config_path = sys.argv[idx]
        if os.path.exists(config_path):
            config_content = open(config_path).read()
entry = {
    "argv": sys.argv[1:],
    "config_path": config_path,
    "config_content": config_content,
}
if log:
    with open(log, "a") as f:
        f.write(json.dumps(entry) + "\\n")
sys.exit(int(os.environ.get("FAKE_DENY_EXIT", "0")))
''')
        self.fake_deny.chmod(0o755)

    def run_script(self, *args, env_overrides=None):
        env = dict(self.env)
        if env_overrides:
            env.update(env_overrides)
        return subprocess.run(
            ["/bin/bash", str(SCRIPTS / "cargo-deny.sh"), *args],
            env=env,
            text=True,
            capture_output=True,
        )

    def test_default_invocation_passes_through(self):
        result = self.run_script("check")
        self.assertEqual(result.returncode, 0, result.stderr)
        entries = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(entries[0]["argv"], ["check"])
        self.assertIsNone(entries[0]["config_path"])

    def test_writable_override_injects_db_path_and_cleans_up(self):
        override_dir = self.root / "writable_dbs"
        override_dir.mkdir()
        result = self.run_script("check", env_overrides={"CARGO_DENY_DB_PATH": str(override_dir)})
        self.assertEqual(result.returncode, 0, result.stderr)
        entries = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn("--config", entries[0]["argv"])
        self.assertIn(f'db-path = "{override_dir}"', entries[0]["config_content"])
        # Temporary config file must be cleaned up on exit
        self.assertFalse(os.path.exists(entries[0]["config_path"]))

    def test_orbit_prefix_override_supported(self):
        override_dir = self.root / "orbit_writable_dbs"
        override_dir.mkdir()
        result = self.run_script("check", env_overrides={"ORBIT_CARGO_DENY_DB_PATH": str(override_dir)})
        self.assertEqual(result.returncode, 0, result.stderr)
        entries = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn(f'db-path = "{override_dir}"', entries[0]["config_content"])

    def test_direct_repo_path_resolves_container(self):
        repo_dir = self.root / "advisory-db-3157b0e258782691"
        (repo_dir / "crates").mkdir(parents=True)
        result = self.run_script("check", env_overrides={"CARGO_DENY_DB_PATH": str(repo_dir)})
        self.assertEqual(result.returncode, 0, result.stderr)
        entries = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn(f'db-path = "{self.root}"', entries[0]["config_content"])

    def test_offline_mode_appends_disable_fetch(self):
        result = self.run_script("check", env_overrides={"CARGO_DENY_OFFLINE": "1"})
        self.assertEqual(result.returncode, 0, result.stderr)
        entries = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn("--disable-fetch", entries[0]["argv"])

    def test_explicit_failure_propagated_without_skip(self):
        result = self.run_script("check", env_overrides={"FAKE_DENY_EXIT": "42"})
        self.assertEqual(result.returncode, 42)

    def test_real_cargo_deny_with_writable_fixture(self):
        if not shutil.which("cargo-deny"):
            self.skipTest("cargo-deny not installed")
        system_advisory = Path.home() / ".cargo/advisory-dbs"
        if not system_advisory.exists():
            self.skipTest("system advisory-dbs snapshot not found")
        fixture = self.root / "fixture-advisory-dbs"
        shutil.copytree(str(system_advisory), str(fixture))
        env = dict(os.environ, CARGO_DENY_DB_PATH=str(fixture), CARGO_DENY_DISABLE_FETCH="1")
        res = subprocess.run(
            ["/bin/bash", str(SCRIPTS / "cargo-deny.sh"), "check", "advisories"],
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(res.returncode, 0, f"stdout: {res.stdout}\nstderr: {res.stderr}")
        self.assertTrue((fixture / "db.lock").exists())

    def test_real_cargo_deny_missing_data_fails_without_skip(self):
        if not shutil.which("cargo-deny"):
            self.skipTest("cargo-deny not installed")
        empty_fixture = self.root / "empty-dbs"
        empty_fixture.mkdir()
        env = dict(os.environ, CARGO_DENY_DB_PATH=str(empty_fixture), CARGO_DENY_DISABLE_FETCH="1")
        res = subprocess.run(
            ["/bin/bash", str(SCRIPTS / "cargo-deny.sh"), "check", "advisories"],
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(res.returncode, 0)
        self.assertIn("error", res.stderr.lower())


if __name__ == "__main__":
    unittest.main()

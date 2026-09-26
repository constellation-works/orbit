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
                        GUARD_TEST_LOG=str(self.log), GUARD_TEST_METADATA=str(self.metadata),
                        GUARD_TEST_BIN=str(self.bin / "fixture-test-bin"))
        self.write_executable(self.bin / "cargo", '''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(sys.argv[1:]) + "\\n")
if sys.argv[1] == "metadata":
    print(open(os.environ["GUARD_TEST_METADATA"]).read())
elif sys.argv[1] == "test" and "--message-format" in sys.argv:
    # `--no-run --message-format json`: report one test binary per package
    # named in the fixture metadata, the way a workspace build does.
    for package in json.load(open(os.environ["GUARD_TEST_METADATA"]))["packages"]:
        print(json.dumps({"reason": "compiler-artifact", "package_id": package["id"],
                          "profile": {"test": True}, "executable": os.environ["GUARD_TEST_BIN"]}))
elif sys.argv[1] == "test":
    print("fixture_test: test")
''')
        # Stands in for a compiled test binary: logs its argv like the cargo
        # stub and lists one libtest-style test.
        self.write_executable(self.bin / "fixture-test-bin", '''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(["fixture-test-bin"] + sys.argv[1:]) + "\\n")
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
        self.metadata.write_text(json.dumps(dict(
            packages=[dict(id="path+file:///fixture/orbit-types#0.1.0", name="orbit-types")])))
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

    def test_fast_invokes_web_blocking_handler_check(self):
        self.prepare_ci()
        self.write_executable(
            self.scripts / "check-web-blocking-handlers.py",
            '''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(["check-web-blocking-handlers.py"] + sys.argv[1:]) + "\\n")
''',
        )
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn(["check-web-blocking-handlers.py"], calls)

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

    def test_fast_invokes_dashboard_vendor_check(self):
        self.prepare_ci()
        self.write_executable(
            self.scripts / "check-dashboard-vendor.py",
            '''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(["check-dashboard-vendor.py"] + sys.argv[1:]) + "\\n")
''',
        )
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn(["check-dashboard-vendor.py"], calls)

    def test_full_still_checks_workflow_test_matches(self):
        # The full run lists the workflow's filtered tests from one workspace
        # build (the artifacts the nextest pass reuses), never from a
        # per-package `cargo test -p` build. [DANI-10428]
        self.prepare_ci()
        result = self.run_guard("ci-guardrails.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn(["test", "--workspace", "--lib", "--bins", "--tests", "--locked", "--no-run",
                       "--message-format", "json"], calls)
        self.assertIn(["fixture-test-bin", "--list", "fixture_test"], calls)
        self.assertNotIn("-p", [argument for call in calls for argument in call])

    def test_macos_check_defaults_to_per_package_listing(self):
        # Without --workspace-build (the macOS job and local runs, whose `-p`
        # artifacts are already warm) the per-package listing is unchanged.
        self.prepare_ci()
        result = self.run_guard("check-ci-macos.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(calls, [["test", "-p", "orbit-types", "--locked", "fixture_test", "--", "--list"]])

    def test_macos_check_rejects_unknown_flags(self):
        self.prepare_ci()
        result = self.run_guard("check-ci-macos.sh", "--fast")
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage:", result.stderr)

    def test_full_invokes_cargo_deny_guard(self):
        self.prepare_ci()
        # The gate is soft-presence: stub the tool so the test does not depend
        # on the host having cargo-deny installed.
        self.write_executable(self.bin / "cargo-deny", "#!/bin/bash\nexit 0\n")
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
        (self.root / "Cargo.toml").write_text(
            '[workspace]\n[workspace.dependencies]\ntempfile = "3"\n'
        )
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

    def test_workspace_dependency_must_be_inherited(self):
        self.dependency_result("orbit-common", "orbit-types")
        manifest = self.root / "orbit-common.toml"
        manifest.write_text('[dev-dependencies]\ntempfile = "3"\n')
        result = self.run_guard("check-dependency-direction.sh")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("dev-dependencies.tempfile must inherit", result.stdout)

        manifest.write_text('[dev-dependencies]\ntempfile.workspace = true\n')
        result = self.run_guard("check-dependency-direction.sh")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_goldens_runs_only_the_orbit_cli_snapshot_tests(self):
        result = self.run_guard("check-goldens.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(
            calls,
            [
                ["test", "-p", "orbit-cli", "--bin", "orbit", "help_matches_the_shipped_surface"],
                ["test", "-p", "orbit-cli", "--test", "output_goldens"],
                [
                    "test",
                    "-p",
                    "orbit-cli",
                    "--test",
                    "mcp_roundtrip",
                    "mcp_serve_tools_list_matches_production_snapshot",
                    "--",
                    "--exact",
                ],
            ],
        )

    def test_goldens_update_sets_regeneration_env_vars(self):
        self.write_executable(
            self.bin / "cargo",
            '''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps({
        "argv": sys.argv[1:],
        "help": os.environ.get("ORBIT_UPDATE_HELP_GOLDENS"),
        "output": os.environ.get("ORBIT_UPDATE_OUTPUT_GOLDENS"),
        "mcp": os.environ.get("ORBIT_MCP_UPDATE_SNAPSHOT"),
    }) + "\\n")
''',
        )
        result = self.run_guard("check-goldens.sh", "--update")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(len(calls), 3)
        for call in calls:
            self.assertEqual(call["help"], "1")
            self.assertEqual(call["output"], "1")
            self.assertEqual(call["mcp"], "1")

    def test_goldens_rejects_unknown_flags(self):
        result = self.run_guard("check-goldens.sh", "--fast")
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage: check-goldens.sh [--update]", result.stderr)


class WorkflowActionPinGuardrailTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.scripts = self.root / "scripts"
        self.scripts.mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.log = self.root / "curl_calls.log"
        self.status_map = self.root / "curl_status_map.json"
        self.status_map.write_text("{}")
        self.env = dict(
            os.environ,
            PATH=f"{self.bin}:{os.environ['PATH']}",
            FAKE_CURL_LOG=str(self.log),
            FAKE_CURL_STATUS_MAP_FILE=str(self.status_map),
        )
        self.write_executable(self.bin / "curl", '''#!/usr/bin/env python3
import json, os, sys
url = sys.argv[-1]
log = os.environ.get("FAKE_CURL_LOG")
if log:
    with open(log, "a") as f:
        f.write(url + "\\n")
mapping = json.loads(open(os.environ["FAKE_CURL_STATUS_MAP_FILE"]).read())
status = mapping.get(url)
if status is None or status == "000":
    sys.exit(1)
sys.stdout.write(status)
''')

    def write_executable(self, path, content):
        path.write_text(content)
        path.chmod(0o755)

    def set_statuses(self, mapping):
        self.status_map.write_text(json.dumps(mapping))

    def write_workflow(self, name, uses_lines):
        workflows = self.root / ".github/workflows"
        workflows.mkdir(parents=True, exist_ok=True)
        body = "jobs:\n  job:\n    steps:\n" + "".join(
            f"      - uses: {line}\n" for line in uses_lines
        )
        (workflows / name).write_text(body)

    def run_guard(self, *arguments):
        shutil.copy2(SCRIPTS / "check-workflow-action-pins.sh", self.scripts / "check-workflow-action-pins.sh")
        return subprocess.run(
            ["/bin/bash", str(self.scripts / "check-workflow-action-pins.sh"), *arguments],
            env=self.env, text=True, capture_output=True,
        )

    def test_passes_when_all_pins_resolve(self):
        good_sha = "a" * 40
        self.write_workflow("check.yml", [f"actions/checkout@{good_sha} # v1"])
        self.set_statuses({
            "https://api.github.com": "200",
            f"https://api.github.com/repos/actions/checkout/commits/{good_sha}": "200",
        })
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_fails_when_a_pin_does_not_resolve(self):
        bad_sha = "b" * 40
        self.write_workflow("check.yml", [f"actions/setup-node@{bad_sha} # v7.0.0"])
        self.set_statuses({
            "https://api.github.com": "200",
            f"https://api.github.com/repos/actions/setup-node/commits/{bad_sha}": "422",
        })
        result = self.run_guard()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"actions/setup-node@{bad_sha} does not resolve", result.stderr)
        self.assertIn(".github/workflows/check.yml:", result.stderr)

    def test_skips_without_network_access(self):
        bad_sha = "c" * 40
        self.write_workflow("check.yml", [f"actions/setup-node@{bad_sha} # v7.0.0"])
        self.set_statuses({})
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("skipping pin resolution", result.stderr)

    def test_dedupes_repeated_pins(self):
        sha = "d" * 40
        self.write_workflow(
            "a.yml",
            [f"actions/checkout@{sha} # v1", f"actions/checkout@{sha} # v1"],
        )
        self.set_statuses({
            "https://api.github.com": "200",
            f"https://api.github.com/repos/actions/checkout/commits/{sha}": "200",
        })
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.log.read_text().splitlines()
        commit_calls = [c for c in calls if c.endswith(f"/commits/{sha}")]
        self.assertEqual(len(commit_calls), 1)


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

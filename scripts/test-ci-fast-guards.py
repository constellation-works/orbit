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
        self.targets = self.root / "targets.json"
        self.env["GUARD_TEST_TARGETS"] = str(self.targets)
        # A miniature libtest. Without a targets file every listing reports one
        # `fixture_test`. With one (see `set_targets`) it honours target
        # selection, the positional filter and `--exact`, so a guard that drops
        # any of them lists different tests than the workflow would run.
        self.write_executable(self.bin / "fixture_libtest.py", '''import json, os

def load_targets():
    path = os.environ["GUARD_TEST_TARGETS"]
    return json.load(open(path)) if os.path.exists(path) else None

def matches(test, filters, libtest_args):
    if not filters:
        return True
    return test == filters[0] if "--exact" in libtest_args else filters[0] in test

def list_target(target, filters, libtest_args):
    return [test + ": test" for test in target["tests"] if matches(test, filters, libtest_args)]

def cargo_listing(targets, cargo_args, libtest_args):
    if targets is None:
        return ["fixture_test: test"]
    selectors, filters, index = [], [], 0
    while index < len(cargo_args):
        token = cargo_args[index]
        if token in ("--test", "--bin"):
            selectors.append((token[2:], cargo_args[index + 1]))
            index += 1
        elif token in ("--lib", "--bins"):
            selectors.append((token[2:].rstrip("s"), None))
        elif token == "-p":
            index += 1
        elif not token.startswith("-"):
            filters.append(token)
        index += 1
    lines = []
    for target in targets:
        if selectors and not any(kind == target["kind"] and name in (None, target["name"])
                                 for kind, name in selectors):
            continue
        lines += list_target(target, filters, libtest_args)
    return lines
''')
        self.write_executable(self.bin / "cargo", '''#!/usr/bin/env python3
import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fixture_libtest
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(sys.argv[1:]) + "\\n")
if sys.argv[1] == "metadata":
    print(open(os.environ["GUARD_TEST_METADATA"]).read())
elif sys.argv[1] == "test" and "--message-format" in sys.argv:
    # `--no-run --message-format json`: report the test binaries of every
    # package named in the fixture metadata, the way a workspace build does.
    targets = fixture_libtest.load_targets()
    if targets is None:
        targets = [dict(name="fixture", kind="lib", binary=os.environ["GUARD_TEST_BIN"])]
    for package in json.load(open(os.environ["GUARD_TEST_METADATA"]))["packages"]:
        for target in targets:
            print(json.dumps({"reason": "compiler-artifact", "package_id": package["id"],
                              "profile": {"test": True}, "executable": target["binary"],
                              "target": {"name": target["name"], "kind": [target["kind"]]}}))
elif sys.argv[1] == "test":
    args = sys.argv[2:]
    split = args.index("--") if "--" in args else len(args)
    for line in fixture_libtest.cargo_listing(fixture_libtest.load_targets(), args[:split], args[split:]):
        print(line)
''')
        # Stands in for a compiled test binary: logs its argv like the cargo
        # stub and lists the tests of the target it was built for.
        self.write_executable(self.bin / "fixture-test-bin", '''#!/usr/bin/env python3
import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fixture_libtest
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps([os.path.basename(sys.argv[0])] + sys.argv[1:]) + "\\n")
targets = fixture_libtest.load_targets()
if targets is None:
    print("fixture_test: test")
else:
    target = next(t for t in targets if t["binary"].endswith(os.path.basename(sys.argv[0])))
    args = sys.argv[1:]
    filters = [a for a in args if not a.startswith("-")]
    for line in fixture_libtest.list_target(target, filters, args):
        print(line)
''')
        self.write_executable(self.bin / "rg", "#!/bin/bash\nexit 1\n")

    def write_executable(self, path, content):
        path.write_text(content)
        path.chmod(0o755)

    def set_targets(self, *targets):
        """Give the fixture package these (name, kind, tests) test targets."""
        entries = []
        for name, kind, tests in targets:
            binary = self.bin / f"fixture-test-bin-{name}"
            shutil.copy2(self.bin / "fixture-test-bin", binary)
            entries.append(dict(name=name, kind=kind, tests=tests, binary=str(binary)))
        self.targets.write_text(json.dumps(entries))

    def set_macos_step(self, command):
        workflow = self.root / ".github/workflows/ci-macos.yml"
        workflow.write_text(workflow.read_text().replace(
            "cargo test -p orbit-types fixture_test", command))

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
        workflows.mkdir(parents=True, exist_ok=True)
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

    def test_fast_rejects_shell_sleep_polls(self):
        self.prepare_ci()
        shutil.copy2(SCRIPTS / "check-test-shell-waits.py", self.scripts)
        (self.scripts / "test-shell-waits-allowlist.json").write_text("[]")
        fixture = self.root / "crates/example/tests/fixture.rs"
        fixture.parent.mkdir(parents=True)
        cases = [
            'let script = "while [ ! -e x ]; do sleep 0.1; done";',
            'let script = r#"until [ -e x ]; do /bin/sleep 0.1; done"#;',
            'let script = r#"while [ ! -e x ]; do\n/bin/sleep 0.1\ndone"#;',
            's.push_str("while [ ! -e x ]; do\\n");\n'
            's.push_str("sleep 0.1\\ndone\\n");',
            'let script = "while true; do for n in 1 2; do sleep 0.1; done; done";',
            'let script = "sh -c \'while true; do /bin/sleep 0.1; done\'";',
            'let script = r#"while true; do "/bin/sleep" 0.1; done"#;',
            'let script = "while true; do sh -c \'sleep 0.1\'; done";',
        ]
        for source in cases:
            with self.subTest(source=source):
                fixture.write_text(source)
                result = self.run_guard("ci-guardrails.sh", "--fast")
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("fixture.rs:", result.stderr)
                self.assertIn("shell sleep poll", result.stderr)
        # A sibling unit-test path receives the same protection.
        fixture.write_text("")
        fixture = self.root / "crates/example/src/process/tests/wait.rs"
        fixture.parent.mkdir(parents=True)
        fixture.write_text(cases[0])
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_fast_allows_nonpolling_sleep_and_narrow_reasoned_exception(self):
        self.prepare_ci()
        shutil.copy2(SCRIPTS / "check-test-shell-waits.py", self.scripts)
        allowlist = self.scripts / "test-shell-waits-allowlist.json"
        allowlist.write_text("[]")
        fixture = self.root / "crates/example/tests/fixture.rs"
        fixture.parent.mkdir(parents=True)
        fixture.write_text('let script = "sleep 0.1; while true; do read -r x; done";\n'
                           '// "while true; do sleep 1; done"\n'
                           'let prose = "we sleep while waiting";')
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 0, result.stderr)
        fixture.write_text('let script = "while [ ! -e x ]; do sleep 0.1; done";')
        exception = dict(path="crates/example/tests/fixture.rs",
                         loop="while [ ! -e x ]; do", reason="bounded sandbox fixture")
        allowlist.write_text(json.dumps([exception]))
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 0, result.stderr)
        # An exception grants one exact loop, not the entire file or duplicates.
        fixture.write_text(fixture.read_text() + '\nlet other = "until true; do sleep 1; done";')
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 1, result.stderr)
        fixture.write_text("")
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("stale shell-wait exception", result.stderr)
        exception["reason"] = ""
        allowlist.write_text(json.dumps([exception]))
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_fast_propagates_desktop_ui_failure(self):
        self.prepare_ci()
        self.write_executable(self.scripts / "check-desktop-ui.sh", "#!/bin/bash\nexit 17\n")
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 17)

    def test_fast_propagates_doc_link_failure(self):
        self.prepare_ci()
        self.write_executable(self.scripts / "check-doc-links.py", "#!/bin/bash\nexit 18\n")
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 18)

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

    def test_macos_check_forwards_target_selectors_and_exact_to_the_listing(self):
        self.prepare_ci()
        self.set_targets(("process", "test", ["update::runs"]), ("other", "test", ["update::runs"]))
        self.set_macos_step("cargo test --no-fail-fast -p orbit-types --locked update:: --test process"
                            " -- --exact --nocapture")
        result = self.run_guard("check-ci-macos.sh")
        self.assertEqual(result.returncode, 1, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(calls, [["test", "-p", "orbit-types", "--locked", "--test", "process",
                                  "update::", "--", "--list", "--exact"]])

    def test_macos_check_rejects_filter_matching_only_in_another_test_binary(self):
        # `--test process` matches nothing when the module moved to another
        # binary, even though the package still has a matching test elsewhere.
        for workspace_build in ([], ["--workspace-build"]):
            with self.subTest(workspace_build=bool(workspace_build)):
                self.prepare_ci()
                self.set_targets(("process", "test", ["other::runs"]), ("tool", "test", ["update::runs"]))
                self.set_macos_step("cargo test -p orbit-types --locked update:: --test process")
                result = self.run_guard("check-ci-macos.sh", *workspace_build)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("matched zero tests: orbit-types update:: --test process", result.stderr)

    def test_macos_check_rejects_filter_matching_only_as_substring_under_exact(self):
        for workspace_build in ([], ["--workspace-build"]):
            with self.subTest(workspace_build=bool(workspace_build)):
                self.prepare_ci()
                self.set_targets(("tool", "test", ["plugin_cli_group::lockfile_upgrade_renamed"]))
                self.set_macos_step("cargo test -p orbit-types --locked plugin_cli_group::lockfile_upgrade"
                                    " --test tool -- --exact")
                result = self.run_guard("check-ci-macos.sh", *workspace_build)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("matched zero tests", result.stderr)

    def test_macos_check_accepts_filters_that_the_selected_target_runs(self):
        for workspace_build in ([], ["--workspace-build"]):
            with self.subTest(workspace_build=bool(workspace_build)):
                self.prepare_ci()
                self.set_targets(("process", "test", ["update::runs"]),
                                 ("tool", "test", ["plugin_cli_group::lockfile_upgrade"]))
                self.set_macos_step("cargo test -p orbit-types --locked update:: --test process\n"
                                    "      - run: cargo test -p orbit-types --locked"
                                    " plugin_cli_group::lockfile_upgrade --test tool -- --exact")
                result = self.run_guard("check-ci-macos.sh", *workspace_build)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_macos_check_rejects_missing_selected_target_in_workspace_build(self):
        self.prepare_ci()
        self.set_targets(("tool", "test", ["update::runs"]))
        self.set_macos_step("cargo test -p orbit-types --locked update:: --test process")
        result = self.run_guard("check-ci-macos.sh", "--workspace-build")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("no test binary matching --test process", result.stderr)

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
        result = self.dependency_result("orbit-cli", "orbit-engine")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must remain dev-only", result.stdout)

    def test_dev_only_dependency_accepts_test_edge(self):
        result = self.dependency_result("orbit-cli", "orbit-engine", "dev")
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

    def test_goldens_runs_mcp_conformance_and_cli_snapshot_tests(self):
        result = self.run_guard("check-goldens.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(
            calls,
            [
                ["test", "-p", "orbit-core", "--test", "ci_failure_goldens"],
                [
                    "test",
                    "-p",
                    "orbit-tools",
                    "--test",
                    "tools",
                    "--",
                    "public_tool_surface::github_log_goldens::",
                    "mcp_definitions::",
                ],
                ["test", "-p", "orbit-cli", "--test", "output", "--", "help_goldens::", "output_goldens::"],
                [
                    "test",
                    "-p",
                    "orbit-cli",
                    "--test",
                    "mcp",
                    "mcp_roundtrip::mcp_serve_tools_list_matches_production_snapshot",
                    "--",
                    "--exact",
                ],
                ["test", "-p", "orbit-exec", "--test", "sandbox_profile_goldens"],
                ["test", "-p", "orbit-core", "--test", "sandbox_profile_goldens"],
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
        "sandbox": os.environ.get("ORBIT_UPDATE_SANDBOX_GOLDENS"),
        "logs": os.environ.get("ORBIT_UPDATE_LOG_GOLDENS"),
    }) + "\\n")
''',
        )
        result = self.run_guard("check-goldens.sh", "--update")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(len(calls), 6)
        for call in calls:
            self.assertEqual(call["help"], "1")
            self.assertEqual(call["output"], "1")
            self.assertEqual(call["mcp"], "1")
            self.assertEqual(call["sandbox"], "1")
            self.assertEqual(call["logs"], "1")

    def test_goldens_rejects_unknown_flags(self):
        result = self.run_guard("check-goldens.sh", "--fast")
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage: check-goldens.sh [--update]", result.stderr)


class DocLinkGuardrailTests(unittest.TestCase):
    """Exercise the checker through its executable boundary in real Git repos."""

    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="doc-link-fixture-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.env = dict(os.environ)
        for name in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"):
            self.env.pop(name, None)
        subprocess.run(["git", "init", "-q", str(self.root)], env=self.env, check=True)
        self.write("scripts/check-doc-links.py", (SCRIPTS / "check-doc-links.py").read_text())

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def run_checker(self):
        subprocess.run(["git", "add", "."], cwd=self.root, env=self.env, check=True)
        return subprocess.run(["python3", str(self.root / "scripts/check-doc-links.py")],
                              env=self.env, text=True, capture_output=True)

    def test_valid_links_slugs_and_examples(self):
        self.write("crates/demo/src/lib.rs", "// fixture source\n")
        self.write("docs/target.md", "# `snake_case` & *Details*\n# Repeat\n# Repeat\n"
                   "# Repeat-2\n# Repeat\nSetext title\n============\n"
                   "<a name=\"custom\"></a>\n# Ελληνικά\n")
        self.write("docs/asset (one).svg", "<svg/>\n")
        self.write("docs/guide.md", "# Guide\n[local](#guide)\n"
                   "[`code label`](target.md#snake_case--details)\n"
                   "[duplicate](target.md#repeat-3)\n[setext](target.md#setext-title)\n"
                   "[Unicode](target.md#%CE%B5%CE%BB%CE%BB%CE%B7%CE%BD%CE%B9%CE%BA%CE%AC)\n"
                   "[custom](target.md#custom)\n![image](<asset (one).svg> \"title\")\n"
                   "[reference][target]\n[target]: target.md#repeat-1\n"
                   "`crates/demo/src/lib.rs` and `crates/demo/src/lib.rs::item`\n"
                   "`[example](missing.md)`\n~~~md\n[example](missing.md)\n"
                   "`crates/missing.rs`\n~~~\n\n    [example](missing.md)\n"
                   "<!-- [comment](missing.md) -->\n"
                   "\\[escaped](missing.md)\n"
                   "[external](https://example.com/missing.md#missing)\n")
        result = self.run_checker()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("checked", result.stdout)

    def test_broken_link_anchor_and_source_path_name_each_line(self):
        self.write("docs/target.md", "# Existing heading\n")
        self.write("docs/guide.md", "[good](target.md#existing-heading)\n"
                   "[broken](missing.md)\n[anchor](target.md#missing)\n"
                   "`crates/demo/src/missing.rs`\n\n- List item\n"
                   "    [nested](missing-nested.md)\n")
        result = self.run_checker()
        self.assertEqual(result.returncode, 1, result.stderr)
        for line, reason, target in (
                (2, "missing link target", "missing.md"),
                (3, "missing heading anchor", "target.md#missing"),
                (4, "missing tracked source path", "crates/demo/src/missing.rs"),
                (7, "missing link target", "missing-nested.md")):
            self.assertIn(f"docs/guide.md:{line}: {reason}: {target}", result.stderr)

    def test_untracked_source_is_rejected_and_excluded_docs_are_ignored(self):
        for name in ("docs/design/demo/4_decisions.md", "docs/design/_templates/example.md",
                     "docs/design/CONVENTIONS.md", "docs/rca/record.md", "CHANGELOG.md",
                     "crates/demo/tests/fixtures/example.md"):
            self.write(name, "[historical](missing.md)\n`crates/missing.rs`\n")
        self.write("docs/guide.md", "`crates/demo/src/untracked.rs`\n")
        subprocess.run(["git", "add", "."], cwd=self.root, env=self.env, check=True)
        self.write("crates/demo/src/untracked.rs", "// untracked\n")
        result = subprocess.run(["python3", str(self.root / "scripts/check-doc-links.py")],
                                env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 1, result.stderr)
        errors = [line for line in result.stderr.splitlines() if ":1:" in line]
        self.assertEqual(errors, ["docs/guide.md:1: missing tracked source path: "
                                  "crates/demo/src/untracked.rs"])

    def test_website_routes_resolve_to_source_and_validate_anchors(self):
        self.write("website/src/content/docs/how-to/guide.md",
                   "[page](../../reference/config/#snake_case)\n[index](../)\n")
        self.write("website/src/content/docs/how-to/index.md", "# How to\n")
        self.write("website/src/content/docs/reference/config.md", "# snake_case\n")
        result = self.run_checker()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.write("website/src/content/docs/reference/config.md", "# Renamed\n")
        result = self.run_checker()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("guide.md:1: missing heading anchor", result.stderr)


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
        self.env.pop("ORBIT_STRICT_WORKFLOW_ACTION_PINS", None)
        self.write_executable(self.bin / "curl", '''#!/usr/bin/env python3
import json, os, sys
url = sys.argv[-1]
log = os.environ.get("FAKE_CURL_LOG")
if log:
    with open(log, "a") as f:
        f.write(url + "\\n")
    with open(log) as f:
        attempt = sum(line.rstrip("\\n") == url for line in f)
else:
    attempt = 1
mapping = json.loads(open(os.environ["FAKE_CURL_STATUS_MAP_FILE"]).read())
status = mapping.get(url)
if isinstance(status, list):
    status = status[min(attempt - 1, len(status) - 1)]
if status is None:
    status = "000"
sys.stdout.write(status)
if status == "000":
    sys.exit(1)
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

    def run_guard(self, *arguments, env_overrides=None):
        shutil.copy2(SCRIPTS / "check-workflow-action-pins.sh", self.scripts / "check-workflow-action-pins.sh")
        env = dict(self.env)
        if env_overrides:
            env.update(env_overrides)
        return subprocess.run(
            ["/bin/bash", str(self.scripts / "check-workflow-action-pins.sh"), *arguments],
            env=env, text=True, capture_output=True,
        )

    def pin_url(self, owner, sha):
        return f"https://api.github.com/repos/{owner}/commits/{sha}"

    def pin_call_count(self, url):
        return sum(line == url for line in self.log.read_text().splitlines())

    def test_passes_when_all_pins_resolve(self):
        good_sha = "a" * 40
        self.write_workflow("check.yml", [f"actions/checkout@{good_sha} # v1"])
        self.set_statuses({
            self.pin_url("actions/checkout", good_sha): "200",
        })
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_fails_when_a_pin_does_not_resolve(self):
        bad_sha = "b" * 40
        self.write_workflow("check.yml", [f"actions/setup-node@{bad_sha} # v7.0.0"])
        url = self.pin_url("actions/setup-node", bad_sha)
        for status in ("404", "422"):
            with self.subTest(status=status):
                self.log.write_text("")
                self.set_statuses({url: status})
                result = self.run_guard()
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(f"actions/setup-node@{bad_sha} does not resolve (status {status})", result.stderr)
                self.assertIn(".github/workflows/check.yml:", result.stderr)
                self.assertEqual(self.pin_call_count(url), 1)

    def test_retries_curl_000_then_skips_with_warning(self):
        bad_sha = "c" * 40
        self.write_workflow("check.yml", [f"actions/setup-node@{bad_sha} # v7.0.0"])
        url = self.pin_url("actions/setup-node", bad_sha)
        self.set_statuses({url: ["000", "000", "000"]})
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("inconclusive resolving", result.stderr)
        self.assertIn("status 000 after 3 attempts", result.stderr)
        self.assertIn("skipping", result.stderr)
        self.assertEqual(self.pin_call_count(url), 3)

    def test_retries_rate_limits_and_server_errors_then_skips(self):
        sha = "e" * 40
        self.write_workflow("check.yml", [f"actions/checkout@{sha} # v1"])
        url = self.pin_url("actions/checkout", sha)
        for status in ("403", "429", "500", "503"):
            with self.subTest(status=status):
                self.log.write_text("")
                self.set_statuses({url: [status, status, status]})
                result = self.run_guard()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(f"status {status} after 3 attempts", result.stderr)
                self.assertIn("skipping", result.stderr)
                self.assertEqual(self.pin_call_count(url), 3)

    def test_strict_mode_fails_when_result_remains_inconclusive(self):
        sha = "f" * 40
        self.write_workflow("check.yml", [f"actions/checkout@{sha} # v1"])
        url = self.pin_url("actions/checkout", sha)
        self.set_statuses({url: ["403", "403", "403"]})
        result = self.run_guard(env_overrides={"ORBIT_STRICT_WORKFLOW_ACTION_PINS": "1"})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("strict mode requires resolution", result.stderr)
        self.assertEqual(self.pin_call_count(url), 3)

    def test_dedupes_repeated_pins(self):
        sha = "d" * 40
        self.write_workflow(
            "a.yml",
            [f"actions/checkout@{sha} # v1", f"actions/checkout@{sha} # v1"],
        )
        self.set_statuses({
            self.pin_url("actions/checkout", sha): "200",
        })
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.log.read_text().splitlines()
        commit_calls = [c for c in calls if c.endswith(f"/commits/{sha}")]
        self.assertEqual(len(commit_calls), 1)


class WorkflowYamlGuardrailTests(unittest.TestCase):
    def setUp(self):
        try:
            import yaml  # noqa: F401
        except ImportError:
            self.skipTest("PyYAML not installed; the guard skips locally without it")
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "scripts").mkdir()
        self.workflows = self.root / ".github/workflows"
        self.workflows.mkdir(parents=True)
        shutil.copy2(SCRIPTS / "check-workflow-yaml.py", self.root / "scripts")

    def write_workflow(self, name, run):
        (self.workflows / name).write_text(
            "on: push\njobs:\n  test:\n    steps:\n      - name: filtered tests\n"
            f"        run: {run}\n")

    def run_guard(self):
        return subprocess.run(
            ["python3", str(self.root / "scripts/check-workflow-yaml.py")],
            text=True, capture_output=True,
        )

    def test_passes_when_every_workflow_parses(self):
        self.write_workflow("ci.yml", "cargo test -p orbit-cli --locked")
        self.write_workflow(
            "ci-macos.yaml", "|\n          cargo test -p orbit-cli --locked generation_root::")
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("2 workflow files parsed", result.stdout)

    def test_trailing_colon_filter_fails_with_its_line(self):
        # A plain scalar ending in `:` is a mapping key; GitHub then creates
        # no jobs for the workflow and the leg silently stops running.
        self.write_workflow("ci-macos.yml", "cargo test -p orbit-cli --locked generation_root::")
        result = self.run_guard()
        self.assertEqual(result.returncode, 1)
        self.assertIn(".github/workflows/ci-macos.yml:6: invalid YAML", result.stderr)

    def test_workflow_without_jobs_fails(self):
        (self.workflows / "empty.yml").write_text("on: push\n")
        result = self.run_guard()
        self.assertEqual(result.returncode, 1)
        self.assertIn("empty.yml: a workflow must be a mapping with a `jobs` mapping",
                      result.stderr)

    def test_homebrew_run_expressions_fail_but_env_values_pass(self):
        import yaml

        expression = "${{ needs.publish-release.outputs.version }}"
        workflow = dict(jobs={"bump-homebrew-tap": dict(steps=[
            dict(env=dict(VERSION=expression), run='git commit -m "orbit v$VERSION"'),
        ])})
        path = self.workflows / "release.yml"
        path.write_text(yaml.safe_dump(workflow))
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)

        for run in (f'VERSION="{expression}"', f'git commit -m "orbit v{expression}"',
                    f'echo safe\n# comment\necho "{expression}"'):
            with self.subTest(run=run):
                workflow["jobs"]["bump-homebrew-tap"]["steps"][0]["run"] = run
                path.write_text(yaml.safe_dump(workflow))
                result = self.run_guard()
                self.assertEqual(result.returncode, 1,
                                 "Homebrew scripts must reject expression substitution before "
                                 "Bash parses tag-derived metadata")
                self.assertIn("bump-homebrew-tap step 1", result.stderr)


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

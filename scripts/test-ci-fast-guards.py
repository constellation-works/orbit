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


if __name__ == "__main__":
    unittest.main()

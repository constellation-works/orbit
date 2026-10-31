#!/usr/bin/env python3
"""Exercise the CodeQL naming guard in disposable repositories."""

from pathlib import Path
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
GIT_ENV_KEYS = ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE")


def clean_env():
    return {key: value for key, value in os.environ.items() if key not in GIT_ENV_KEYS}


class ProductionPathsTests(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="codeql-path-fixture-", dir=scratch)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.write("scripts/check-codeql-production-paths.py", "")
        shutil.copy2(ROOT / "scripts/check-codeql-production-paths.py",
                     self.root / "scripts/check-codeql-production-paths.py")
        self.config('**/*_test.rs', '**/*_tests.rs', '**/test_support.rs',
                    '**/tests/**', 'crates/**/examples/**', '**/benches/**')
        subprocess.run(["git", "init", "-q", str(self.root)], check=True, env=clean_env())

    def write(self, name, content):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
        return path

    def config(self, *patterns):
        self.write(".github/codeql/codeql-config.yml",
                   "name: fixture\npaths-ignore:\n" +
                   "".join(f'  - "{pattern}"\n' for pattern in patterns))

    def run_guard(self):
        return subprocess.run([sys.executable, str(self.root / "scripts/check-codeql-production-paths.py")],
                              capture_output=True, text=True, cwd=self.root, env=clean_env())

    def assert_passes(self):
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def assert_excluded(self, path, pattern):
        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertTrue(
            any(path in line and pattern in line for line in result.stderr.splitlines()),
            f"no single line names both {path!r} and {pattern!r}: {result.stderr}")

    def test_tracked_and_new_production_helpers_fail_with_path_and_pattern(self):
        for filename, pattern in (("child_test.rs", "**/*_test.rs"),
                                  ("child_tests.rs", "**/*_tests.rs"),
                                  ("test_support.rs", "**/test_support.rs")):
            for tracked in (False, True):
                with self.subTest(filename=filename, tracked=tracked):
                    path = f"crates/demo/src/test_env/{filename}"
                    self.write(path, "pub fn child() {}\n")
                    self.write("crates/demo/src/test_env/mod.rs", f"mod {Path(filename).stem};\n")
                    if tracked:
                        subprocess.run(["git", "add", path], cwd=self.root, check=True, env=clean_env())
                    self.assert_excluded(path, pattern)

    def test_non_excluded_production_and_test_target_directories_pass(self):
        for path in ("crates/demo/src/child.rs", "crates/demo/tests/child_test.rs",
                     "crates/demo/src/tests/test_support.rs", "crates/demo/examples/child_test.rs",
                     "crates/demo/benches/child_test.rs"):
            self.write(path, "fn fixture() {}\n")
        self.assert_passes()

    def test_direct_gates_and_owners_pass(self):
        owners = ("crates/demo/src/lib.rs", "crates/demo/src/main.rs",
                  "crates/demo/src/helper/mod.rs", "crates/demo/src/helper.rs")
        for number, original_owner in enumerate(owners):
            for variant, gate in enumerate(('#[cfg(test)]', '#[cfg(feature = "test-support")]')):
                owner = original_owner.replace("/demo/", f"/demo{number}_{variant}/")
                with self.subTest(owner=owner, gate=gate):
                    base = Path(owner).parent if Path(owner).stem in ("lib", "main", "mod") else Path(owner).with_suffix("")
                    self.write(str(base / "test_support.rs"), "fn fixture() {}\n")
                    self.write(owner, gate + '\n#[allow(dead_code)]\npub(crate) mod test_support;\n')
                    self.assert_passes()

    def test_path_attribute_and_directory_module_gate_pass(self):
        self.write("crates/demo/src/child_test.rs", "fn fixture() {}\n")
        self.write("crates/demo/src/lib.rs", '#[cfg(test)]\n#[path = "child_test.rs"]\nmod renamed;\n')
        self.assert_passes()
        self.config("**/test_support/**")
        self.write("crates/demo/src/test_support/mod.rs", "fn fixture() {}\n")
        self.write("crates/demo/src/lib.rs", "#[cfg(test)]\nmod test_support;\n")
        self.assert_passes()

    def test_comments_literals_other_items_and_conditional_attributes_cannot_exempt(self):
        path = "crates/demo/src/child_test.rs"
        self.write(path, "pub fn child() {}\n")
        for source in (
            '// #[cfg(test)]\nmod child_test;',
            '/* outer /* #[cfg(test)] */ mod child_test; */\nmod child_test;',
            'const DOC: &str = "#[cfg(test)] mod child_test;";\nmod child_test;',
            'const DOC: &str = r##"#[cfg(test)] mod child_test;"##;\nmod child_test;',
            'const DOC: &[u8] = br##"" #[cfg(test)] mod child_test; ""##;\nmod child_test;',
            '#[cfg(test)] fn test() {}\nmod child_test;',
            '#[cfg(any(test, feature = "production"))]\nmod child_test;',
            '#[cfg_attr(test, cfg(test))]\nmod child_test;',
            '#[cfg(not(test))]\nmod child_test;',
            '#[cfg(test)]\nmod child_test;\nmod child_test;',
        ):
            with self.subTest(source=source):
                self.write("crates/demo/src/lib.rs", source)
                self.assert_excluded(path, "**/*_test.rs")

    def test_patterns_come_from_config_and_match_root_and_nested_paths(self):
        self.config("**/hidden.rs", "crates/*/src/private/**")
        self.write("hidden.rs", "fn fixture() {}")
        self.write("crates/demo/src/private/helper.rs", "fn fixture() {}")
        self.assert_excluded("hidden.rs", "**/hidden.rs")
        self.assert_excluded("crates/demo/src/private/helper.rs", "crates/*/src/private/**")

    def test_ignored_build_output_is_not_source_but_tracked_output_fails(self):
        self.config("target/**")
        self.write(".gitignore", "/target/\n")
        self.write("target/generated.rs", "fn fixture() {}")
        self.assert_passes()
        subprocess.run(["git", "add", "-f", "target/generated.rs"], cwd=self.root, check=True, env=clean_env())
        self.assert_excluded("target/generated.rs", "target/**")

    def test_malformed_config_fails_closed(self):
        for config in ('paths-ignore: ["**/*_test.rs"]\n',
                       'paths-ignore:\n  - "**/*_test.rs"\npaths-ignore:\n  - "other"\n',
                       'paths-ignore:\n  - "src/**test.rs"\n'):
            with self.subTest(config=config):
                self.write(".github/codeql/codeql-config.yml", config)
                self.assertEqual(self.run_guard().returncode, 1)

    def test_ci_fast_routes_the_real_guard_and_propagates_failure(self):
        shell = (ROOT / "scripts/ci-guardrails.sh").read_text()
        for name in re.findall(r'\$repo_root/scripts/([\w.-]+)', shell):
            if name in {"check-codeql-production-paths.py", "build-budget.py"}:
                continue
            stub = self.write(f"scripts/{name}", "#!/bin/sh\nexit 0\n")
            stub.chmod(0o755)
        # The guard pool sizes itself from the real budget query, so keep the real script.
        shutil.copy2(ROOT / "scripts/build-budget.py", self.root / "scripts/build-budget.py")
        shutil.copy2(ROOT / "scripts/ci-guardrails.sh", self.root / "scripts/ci-guardrails.sh")
        cargo = self.write("bin/cargo", "#!/bin/sh\nexit 0\n")
        cargo.chmod(0o755)
        env = dict(clean_env(), PATH=f"{cargo.parent}:{os.environ['PATH']}",
                   ORBIT_BUILD_BUDGET_DIR=str(self.root / "budget"))
        command = ["bash", str(self.root / "scripts/ci-guardrails.sh"), "--fast"]
        self.write("crates/demo/src/child_test.rs", "pub fn child() {}\n")
        self.write("crates/demo/src/lib.rs", "#[cfg(test)]\nmod child_test;\n")
        result = subprocess.run(command, env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.write("crates/demo/src/lib.rs", "mod child_test;\n")
        result = subprocess.run(command, env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("crates/demo/src/child_test.rs", result.stderr)
        self.assertIn("**/*_test.rs", result.stderr)


if __name__ == "__main__":
    unittest.main()

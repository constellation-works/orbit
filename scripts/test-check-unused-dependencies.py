#!/usr/bin/env python3
"""Self-test for check-unused-dependencies.py against synthetic workspaces."""

from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent
CHECKER = SCRIPTS / "check-unused-dependencies.py"
REPO_ROOT = SCRIPTS.parent


class UnusedDependencyGuardTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "Cargo.toml").write_text('[workspace]\nmembers = ["crates/fixture"]\n\n[workspace.dependencies]\nserde = "1"\n')

    def write_crate(self, manifest, sources):
        crate = self.root / "crates" / "fixture"
        (crate / "src").mkdir(parents=True, exist_ok=True)
        (crate / "Cargo.toml").write_text(manifest)
        for relative, text in sources.items():
            path = crate / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)

    def run_checker(self, root=None):
        result = subprocess.run(
            [sys.executable, str(CHECKER), str(root or self.root)],
            capture_output=True,
            text=True,
            check=False,
        )
        return result.returncode, result.stdout

    def test_unused_normal_dependency_fails(self):
        self.write_crate('[package]\nname = "fixture"\n\n[dependencies]\nserde = { workspace = true }\n',
                         {"src/lib.rs": "pub fn value() -> u8 { 1 }\n"})
        code, output = self.run_checker()
        self.assertEqual(code, 1, output)
        self.assertIn("dependencies.serde is unused", output)

    def test_normal_dependency_used_only_by_tests_is_misplaced(self):
        self.write_crate('[package]\nname = "fixture"\n\n[dependencies]\nserde = { workspace = true }\n',
                         {"src/lib.rs": "pub fn value() -> u8 { 1 }\n",
                          "tests/serde.rs": "use serde::Serialize;\n"})
        code, output = self.run_checker()
        self.assertEqual(code, 1, output)
        self.assertIn("used only by test targets; move it to [dev-dependencies]", output)

    def test_target_specific_unused_dependency_fails(self):
        self.write_crate('[package]\nname = "fixture"\n\n[target.\'cfg(unix)\'.dependencies]\nserde = { workspace = true }\n',
                         {"src/lib.rs": "pub fn value() -> u8 { 1 }\n"})
        code, output = self.run_checker()
        self.assertEqual(code, 1, output)
        self.assertIn("target.cfg(unix).dependencies.serde is unused", output)

    def test_dev_dependency_referenced_from_tests_passes(self):
        self.write_crate('[package]\nname = "fixture"\n\n[dev-dependencies]\nserde = { workspace = true }\n',
                         {"src/lib.rs": "pub fn value() -> u8 { 1 }\n",
                          "tests/serde.rs": "use serde::Serialize;\n"})
        code, output = self.run_checker()
        self.assertEqual(code, 0, output)

    def test_orphaned_workspace_dependency_fails(self):
        self.write_crate('[package]\nname = "fixture"\n', {"src/lib.rs": "pub fn value() -> u8 { 1 }\n"})
        code, output = self.run_checker()
        self.assertEqual(code, 1, output)
        self.assertIn("[workspace.dependencies] serde is declared by no member", output)

    def test_repository_tree_passes(self):
        code, output = self.run_checker(REPO_ROOT)
        self.assertEqual(code, 0, output)


if __name__ == "__main__":
    unittest.main()

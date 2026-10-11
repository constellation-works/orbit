#!/usr/bin/env python3
"""Run the real `make install` and `make uninstall` recipes into destinations that contain spaces.

The repository Makefile runs from a scratch working directory through the real
build-budget wrapper, with a stub cargo in place of the compiler. The stub answers
`build` with the same compiler-artifact JSON cargo emits, so the recipe's binary
lookup runs for real and nothing compiles. Install tests assert the exact tree the
recipe left behind, so a word-split destination shows up as a sibling directory,
a stray file, or a missing executable.
"""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
MAKEFILE = ROOT / "Makefile"
BUILD_BUDGET = ROOT / "scripts" / "build-budget.py"

# Logs each cargo call with the budget-held flag, writes an executable for the
# requested profile, and reports it the way cargo's JSON messages do.
STUB_CARGO = r"""#!/usr/bin/env bash
set -eu
cmd="$1"
shift
printf '%s held=%s %s\n' "$cmd" "${ORBIT_BUILD_BUDGET_HELD:-}" "$*" >>"$FAKE_CARGO_LOG"
[ "$cmd" = build ] || exit 64
profile=debug
for arg in "$@"; do
  if [ "$arg" = --release ]; then profile=release; fi
done
artifact="$FAKE_TARGET/$profile/orbit"
mkdir -p "$(dirname "$artifact")"
printf '#!/usr/bin/env bash\necho %s-artifact\n' "$profile" >"$artifact"
chmod +x "$artifact"
python3 -c 'import json, sys; print(json.dumps({"reason": "compiler-artifact", "executable": sys.argv[1], "target": {"name": "orbit"}}))' "$artifact"
"""


def tree(root):
    """Every path under `root`, so a stray sibling or split directory is visible."""
    return {path.relative_to(root).as_posix() for path in root.rglob("*")}


class MakeInstallRecipeTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="orbit-make-install-")
        self.addCleanup(temporary.cleanup)
        scratch = Path(temporary.name)
        # The recipe word-splits BUILD_BUDGET and CARGO, so their paths cannot hold whitespace.
        for path in (scratch, BUILD_BUDGET):
            if any(char.isspace() for char in str(path)):
                raise AssertionError(f"{path} contains whitespace; the recipe word-splits BUILD_BUDGET and CARGO")
        # The make working directory doubles as the parent of every destination, so any
        # path a word-split operand resolves relative to it lands where the tree check sees it.
        self.work = scratch / "work"
        self.work.mkdir()
        self.home = scratch / "home"
        self.home.mkdir()
        self.locks = scratch / "locks"
        self.target = scratch / "target"
        self.cargo_log = scratch / "cargo.log"
        self.stub = scratch / "tools" / "cargo"
        self.stub.parent.mkdir()
        self.stub.write_text(STUB_CARGO)
        self.stub.chmod(0o755)
        self.spaced = self.work / "install dir" / "bin"
        self.plain = self.work / "plain" / "bin"

    def run_make(self, goal, destination):
        env = {
            "PATH": os.environ["PATH"],
            "HOME": str(self.home),
            "FAKE_CARGO_LOG": str(self.cargo_log),
            "FAKE_TARGET": str(self.target),
            "ORBIT_BUILD_BUDGET_DIR": str(self.locks),
            "ORBIT_BUILD_SLOTS": "1",
        }
        result = subprocess.run(
            [
                "make", "-f", str(MAKEFILE), "-C", str(self.work), goal,
                f"INSTALL_BIN_DIR={destination}",
                f"BUILD_BUDGET={BUILD_BUDGET}",
                f"CARGO={self.stub}",
            ],
            env=env,
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
        self.assertEqual(result.returncode, 0, f"make {goal} failed:\n{result.stdout}{result.stderr}")
        return result

    def cargo_calls(self):
        return self.cargo_log.read_text().splitlines() if self.cargo_log.exists() else []

    def assert_installed_artifact(self, executable, profile):
        self.assertTrue(executable.is_file(), f"{executable} was not installed")
        self.assertTrue(os.access(executable, os.X_OK), f"{executable} is not executable")
        run = subprocess.run([str(executable)], capture_output=True, text=True, timeout=30, check=False)
        self.assertEqual(run.stdout, f"{profile}-artifact\n", "installed file is not the reported release artifact")

    def test_install_into_spaced_destination_creates_only_the_intended_tree(self):
        self.run_make("install", self.spaced)
        self.assertEqual(tree(self.work), {"install dir", "install dir/bin", "install dir/bin/orbit"})
        self.assert_installed_artifact(self.spaced / "orbit", "release")
        calls = self.cargo_calls()
        self.assertEqual(len(calls), 1, calls)
        self.assertTrue(calls[0].startswith("build held=1 "), "cargo ran outside the build budget")
        self.assertIn("--release", calls[0].split())

    def test_install_into_plain_destination_still_installs(self):
        self.run_make("install", self.plain)
        self.assertEqual(tree(self.work), {"plain", "plain/bin", "plain/bin/orbit"})
        self.assert_installed_artifact(self.plain / "orbit", "release")

    def seed_destination(self, destination):
        """Place an installed executable and a sibling directly, so uninstall is tested alone."""
        destination.mkdir(parents=True)
        (destination / "orbit").write_text("installed\n")
        (destination / "keep").write_text("sibling in the destination\n")

    def test_uninstall_from_spaced_destination_removes_only_the_executable(self):
        self.seed_destination(self.spaced)
        # A word-split uninstall would name this file as its first operand.
        (self.work / "install").write_text("sibling beside the destination\n")
        self.run_make("uninstall", self.spaced)
        self.assertEqual(tree(self.work), {"install", "install dir", "install dir/bin", "install dir/bin/keep"})

    def test_uninstall_from_plain_destination_removes_only_the_executable(self):
        self.seed_destination(self.plain)
        self.run_make("uninstall", self.plain)
        self.assertEqual(tree(self.work), {"plain", "plain/bin", "plain/bin/keep"})


if __name__ == "__main__":
    unittest.main()

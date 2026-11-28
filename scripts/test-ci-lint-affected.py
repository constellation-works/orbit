#!/usr/bin/env python3
"""Exercise diff-scoped lint selection and the real Clippy/rustdoc wrapper gate."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest


SCRIPTS = Path(__file__).resolve().parent
ROOT = SCRIPTS.parent


def clean_git_env():
    env = dict(os.environ)
    for name in (
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "CI_TEST_BASE",
        "RUSTC_WORKSPACE_WRAPPER",
        "CLIPPY_ARGS",
        "RUSTDOCFLAGS",
    ):
        env.pop(name, None)
    env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
    return env


class LintAffectedSelectionTests(unittest.TestCase):
    """Exercise crate selection, wrapper invocation order, and base resolution."""

    def setUp(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="ci-lint-affected-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "scripts").mkdir()
        shutil.copy2(SCRIPTS / "ci-lint-affected.py", self.root / "scripts/ci-lint-affected.py")
        shutil.copy2(SCRIPTS / "require-python.sh", self.root / "scripts/require-python.sh")
        shutil.copy2(ROOT / "Makefile", self.root / "Makefile")
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.log = self.root / "calls.log"
        self.metadata = self.root / "metadata.json"
        self.env = clean_git_env()
        self.env.update(
            PATH=f"{self.bin}:{os.environ['PATH']}",
            GIT_CEILING_DIRECTORIES=str(scratch),
            GUARD_TEST_LOG=str(self.log),
            GUARD_TEST_METADATA=str(self.metadata),
            BUILD_BUDGET=str(self.bin / "build-budget"),
            CARGO="cargo",
        )
        packages = []
        crates = {
            "orbit-core": ([dict(name="orbit_core", kind=["lib"])], []),
            "orbit-web": ([dict(name="orbit_web", kind=["lib"])], ["orbit-core"]),
            "orbit-cli": ([dict(name="orbit", kind=["bin"])], ["orbit-web"]),
            "orbit-tools": ([dict(name="orbit_tools", kind=["lib"])], []),
        }
        for name, (targets, deps) in crates.items():
            crate = self.root / "crates" / name
            (crate / "src").mkdir(parents=True)
            (crate / "Cargo.toml").write_text(f'[package]\nname = "{name}"\nversion = "0.1.0"\n')
            entry = "src/main.rs" if targets[0]["kind"] == ["bin"] else "src/lib.rs"
            (crate / entry).write_text("// baseline\n")
            packages.append(
                dict(
                    id=name,
                    name=name,
                    manifest_path=str(crate / "Cargo.toml"),
                    targets=targets,
                    dependencies=[
                        dict(name=dep, kind=None, path=str(self.root / "crates" / dep))
                        for dep in deps
                    ],
                )
            )
        self.metadata.write_text(json.dumps(dict(packages=packages, workspace_members=sorted(crates))))
        (self.root / "Cargo.toml").write_text("[workspace]\n")
        (self.root / ".gitignore").write_text("bin/\ncalls.log\nmetadata.json\n")
        (self.root / "docs").mkdir()
        (self.root / "docs/guide.md").write_text("Fixture docs\n")
        self.write_executable(
            self.root / "scripts/with-test-build-lint.sh",
            '#!/usr/bin/env python3\n'
            'import json, os, subprocess, sys\n'
            'with open(os.environ["GUARD_TEST_LOG"], "a") as log:\n'
            '    log.write(json.dumps({"wrapper": sys.argv[1:2], "rustdocflags": os.environ.get("RUSTDOCFLAGS")}) + "\\n")\n'
            'assert sys.argv[1] == "--check", sys.argv\n'
            'sys.exit(subprocess.run(sys.argv[2:]).returncode)\n',
        )
        self.write_executable(
            self.bin / "build-budget",
            '#!/usr/bin/env python3\n'
            'import json, os, sys\n'
            'with open(os.environ["GUARD_TEST_LOG"], "a") as log:\n'
            '    log.write(json.dumps({"budget": sys.argv[1:]}) + "\\n")\n'
            'assert sys.argv[1] == "--"\n'
            'os.execvp(sys.argv[2], sys.argv[2:])\n',
        )
        self.write_executable(
            self.bin / "cargo",
            '#!/usr/bin/env python3\n'
            'import json, os, sys\n'
            'args = sys.argv[1:]\n'
            'with open(os.environ["GUARD_TEST_LOG"], "a") as log:\n'
            '    log.write(json.dumps({"cargo": args, "rustdocflags": os.environ.get("RUSTDOCFLAGS")}) + "\\n")\n'
            'if args[0] == "metadata":\n'
            '    print(open(os.environ["GUARD_TEST_METADATA"]).read())\n'
            '    sys.exit(0)\n'
            'fail_at = os.environ.get("GUARD_TEST_FAIL_SUBCMD")\n'
            'if fail_at and fail_at in " ".join(args):\n'
            '    sys.exit(int(os.environ.get("GUARD_TEST_FAIL_EXIT", "17")))\n',
        )
        self.git("init", "--initial-branch=agent-main")
        self.git("add", ".")
        self.git("commit", "-m", "baseline")
        self.base = self.git("rev-parse", "HEAD").stdout.strip()
        self.git("checkout", "-b", "candidate")

    def write_executable(self, path, content):
        path.write_text(content)
        path.chmod(0o755)

    def git(self, *arguments):
        return subprocess.run(
            [
                "git",
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                *arguments,
            ],
            cwd=self.root,
            env=self.env,
            text=True,
            capture_output=True,
            check=True,
        )

    def run_gate(self, *arguments, **extra_env):
        return subprocess.run(
            ["python3", str(self.root / "scripts/ci-lint-affected.py"), *arguments],
            cwd=self.root,
            env=dict(self.env, **extra_env),
            text=True,
            capture_output=True,
        )

    def logged(self):
        if not self.log.exists():
            return []
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def test_no_crate_changed_exits_zero_without_compiling(self):
        (self.root / "docs/guide.md").write_text("Updated docs\n")
        (self.root / "docs/new.md").write_text("Untracked docs\n")
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.logged()
        self.assertEqual([entry["cargo"][0] for entry in calls if "cargo" in entry], ["metadata"])
        self.assertFalse(any("budget" in entry or "wrapper" in entry for entry in calls), calls)

        self.log.unlink()
        make_result = subprocess.run(
            ["make", "ci-lint-affected", f"BUILD_BUDGET={self.bin / 'build-budget'}"],
            cwd=self.root,
            env=self.env,
            text=True,
            capture_output=True,
        )
        self.assertEqual(make_result.returncode, 0, make_result.stderr)
        make_calls = self.logged()
        self.assertEqual([entry["cargo"][0] for entry in make_calls if "cargo" in entry], ["metadata"])

    def test_lib_crate_runs_three_checks_sequentially_for_changed_crates_only(self):
        # Change orbit-core only: its reverse dependents (orbit-web, orbit-cli) must NOT run.
        (self.root / "crates/orbit-core/src/lib.rs").write_text("// changed\n")
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.logged()
        budgets = [entry["budget"] for entry in calls if "budget" in entry]
        self.assertEqual(len(budgets), 3, calls)
        cargo_calls = [
            (entry["cargo"], entry["rustdocflags"])
            for entry in calls
            if "cargo" in entry and entry["cargo"][0] != "metadata"
        ]
        self.assertEqual(
            cargo_calls,
            [
                (["check", "-p", "orbit-core", "--lib", "--bins", "--message-format=json"], None),
                (["check", "-p", "orbit-core", "--all-targets", "--message-format=json"], None),
                (["doc", "--no-deps", "-p", "orbit-core", "--message-format=json"], "-D warnings"),
            ],
        )

    def test_bin_only_crate_uses_bins_flag_and_move_selects_both_crates(self):
        # Untracked file in bin-only orbit-cli.
        (self.root / "crates/orbit-cli/src/extra.rs").write_text("// untracked\n")
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        cargo_calls = [
            entry["cargo"]
            for entry in self.logged()
            if "cargo" in entry and entry["cargo"][0] != "metadata"
        ]
        self.assertEqual(
            cargo_calls,
            [
                ["check", "-p", "orbit-cli", "--bins", "--message-format=json"],
                ["check", "-p", "orbit-cli", "--all-targets", "--message-format=json"],
                ["doc", "--no-deps", "-p", "orbit-cli", "--message-format=json"],
            ],
        )

        # Move a file from orbit-tools to orbit-cli -> both crates selected.
        (self.root / "crates/orbit-cli/src/extra.rs").unlink()
        (self.root / "crates/orbit-tools/src/moved.rs").write_text("pub fn moved() {}\n")
        self.git("add", "crates/orbit-tools/src/moved.rs")
        self.git("commit", "-m", "seed moved file")
        self.base = self.git("rev-parse", "HEAD").stdout.strip()
        self.git("mv", "crates/orbit-tools/src/moved.rs", "crates/orbit-cli/src/moved.rs")
        self.git("commit", "-m", "move file across crates")
        listed = self.run_gate("--base", self.base, "--list")
        self.assertEqual(listed.returncode, 0, listed.stderr)
        self.assertEqual(listed.stdout.splitlines(), ["orbit-cli", "orbit-tools"])

    def test_merge_base_ignores_later_agent_main_commits_and_rejects_bad_base(self):
        # Advance agent-main after branching; only candidate's crate should be selected.
        self.git("checkout", "agent-main")
        (self.root / "crates/orbit-tools/src/lib.rs").write_text("// agent-main moved\n")
        self.git("add", "crates/orbit-tools/src/lib.rs")
        self.git("commit", "-m", "upstream advance")
        self.git("checkout", "candidate")
        (self.root / "crates/orbit-core/src/lib.rs").write_text("// candidate change\n")
        self.git("add", "crates/orbit-core/src/lib.rs")
        self.git("commit", "-m", "candidate change")

        listed = self.run_gate("--list")
        self.assertEqual(listed.returncode, 0, listed.stderr)
        self.assertEqual(listed.stdout.splitlines(), ["orbit-core"])

        explicit = self.run_gate("--list", CI_TEST_BASE="agent-main")
        self.assertEqual(explicit.returncode, 0, explicit.stderr)
        self.assertEqual(explicit.stdout.splitlines(), ["orbit-core"])

        bad = self.run_gate(CI_TEST_BASE="nonexistent-ref")
        self.assertNotEqual(bad.returncode, 0)

    def test_failing_pass_stops_and_exits_nonzero(self):
        (self.root / "crates/orbit-core/src/lib.rs").write_text("// changed\n")
        result = self.run_gate(GUARD_TEST_FAIL_SUBCMD="--all-targets", GUARD_TEST_FAIL_EXIT="23")
        self.assertEqual(result.returncode, 23, result.stderr)
        cargo_calls = [
            entry["cargo"]
            for entry in self.logged()
            if "cargo" in entry and entry["cargo"][0] != "metadata"
        ]
        self.assertEqual(len(cargo_calls), 2, cargo_calls)
        self.assertIn("FAILED orbit-core (all-targets, exit 23)", result.stderr)


class LintAffectedRealCargoTests(unittest.TestCase):
    """Run real `make ci-lint-affected` with real Cargo, Clippy, and rustdoc."""

    def setUp(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="ci-lint-real-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

        (self.root / "scripts").mkdir()
        for name in (
            "ci-lint-affected.py",
            "with-test-build-lint.sh",
            "check-rust-source-order.py",
            "build-budget.py",
            "require-python.sh",
        ):
            shutil.copy2(SCRIPTS / name, self.root / "scripts" / name)
        (self.root / "scripts/rust-source-order-baseline.tsv").write_text("")
        shutil.copy2(ROOT / "Makefile", self.root / "Makefile")
        shutil.copy2(ROOT / "clippy.toml", self.root / "clippy.toml")

        manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
        clippy_lints = "\n".join(
            f'{name} = "{level}"' for name, level in manifest["workspace"]["lints"]["clippy"].items()
        )
        rust_lints = "\n".join(
            f'{name} = "{level}"' for name, level in manifest["workspace"]["lints"]["rust"].items()
        )
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["crates/crate-alpha", "crates/crate-beta"]\nresolver = "2"\n\n'
            f'[workspace.lints.clippy]\n{clippy_lints}\n\n'
            f'[workspace.lints.rust]\n{rust_lints}\n'
        )
        for crate in ("crate-alpha", "crate-beta"):
            crate_dir = self.root / "crates" / crate
            (crate_dir / "src").mkdir(parents=True)
            (crate_dir / "Cargo.toml").write_text(
                f'[package]\nname = "{crate}"\nversion = "0.1.0"\nedition = "2024"\n\n'
                "[lints]\nworkspace = true\n"
            )
        self.alpha = self.root / "crates/crate-alpha/src/lib.rs"
        self.beta = self.root / "crates/crate-beta/src/lib.rs"
        self.clean_alpha = (
            "//! Clean fixture crate.\n\n"
            "/// Returns one.\n"
            "pub fn value() -> u32 {\n"
            "    1\n"
            "}\n"
        )
        self.alpha.write_text(self.clean_alpha)
        # Seed an unused import in unchanged crate-beta at the base so we prove
        # ci-lint-affected only checks crates changed against the base.
        self.beta.write_text("//! Unchanged crate.\n\nuse std::fmt;\n\n/// Returns two.\npub fn other() -> u32 {\n    2\n}\n")
        (self.root / ".gitignore").write_text("target/\nbudget/\n")
        (self.root / "docs").mkdir()
        (self.root / "docs/notes.md").write_text("Initial notes\n")

        self.env = clean_git_env()
        self.env.update(
            GIT_CEILING_DIRECTORIES=str(scratch),
            CARGO_TARGET_DIR=str(self.root / "target"),
            CARGO_NET_OFFLINE="true",
            ORBIT_BUILD_BUDGET_DIR=str(self.root / "budget"),
        )
        for args in (
            ["init", "-q", "-b", "agent-main"],
            ["add", "."],
            ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
             "-c", "commit.gpgsign=false", "commit", "-qm", "base"],
            ["checkout", "-q", "-b", "candidate"],
        ):
            subprocess.run(["git", *args], cwd=self.root, env=self.env, check=True, capture_output=True)

    def run_make(self):
        return subprocess.run(
            ["make", "ci-lint-affected"],
            cwd=self.root,
            env=self.env,
            text=True,
            capture_output=True,
        )

    def test_real_make_ci_lint_affected_catches_unused_imports_and_rustdoc_warnings(self):
        # 1. Docs-only change does nothing and exits 0 without compiling.
        (self.root / "docs/notes.md").write_text("Docs-only change\n")
        noop = self.run_make()
        self.assertEqual(noop.returncode, 0, noop.stdout + noop.stderr)
        self.assertFalse((self.root / "target").exists(), "docs-only run compiled Cargo targets")

        # 2. Seeded unused import in changed crate-alpha fails production lint check.
        self.alpha.write_text(
            "//! Seeded unused import.\n\nuse std::fmt;\n\n/// Returns one.\npub fn value() -> u32 {\n    1\n}\n"
        )
        unused_prod = self.run_make()
        self.assertNotEqual(unused_prod.returncode, 0, unused_prod.stdout + unused_prod.stderr)
        self.assertIn("FAILED crate-alpha (production", unused_prod.stderr)
        self.assertIn("unused import", unused_prod.stderr)
        self.assertIn("Rust lint gate:", unused_prod.stderr)

        # 3. Seeded declaration-order violation fails Clippy lint check.
        self.alpha.write_text(
            "//! Seeded ordering violation.\n\n"
            "/// Returns one.\n"
            "pub fn value() -> u32 {\n    1\n}\n\n"
            "/// Late struct.\n"
            "pub struct Late;\n"
        )
        order_fail = self.run_make()
        self.assertNotEqual(order_fail.returncode, 0, order_fail.stdout + order_fail.stderr)
        self.assertIn("FAILED crate-alpha (production", order_fail.stderr)
        self.assertIn("arbitrary_source_item_ordering", order_fail.stderr)

        # 4. Seeded unused import inside #[cfg(test)] passes production and fails all-targets.
        self.alpha.write_text(
            self.clean_alpha
            + "\n#[cfg(test)]\nmod tests {\n    use std::fmt;\n\n    #[test]\n    fn smoke() {}\n}\n"
        )
        unused_test = self.run_make()
        self.assertNotEqual(unused_test.returncode, 0, unused_test.stdout + unused_test.stderr)
        self.assertIn("FAILED crate-alpha (all-targets", unused_test.stderr)
        self.assertIn("unused import", unused_test.stderr)

        # 5. Seeded private intra-doc link passes production/all-targets and fails rustdoc.
        self.alpha.write_text(
            "//! Seeded private intra-doc link.\n\n"
            "fn private_helper() -> u32 {\n"
            "    1\n"
            "}\n\n"
            "/// Links to [`private_helper`].\n"
            "pub fn value() -> u32 {\n"
            "    private_helper()\n"
            "}\n"
        )
        rustdoc_fail = self.run_make()
        self.assertNotEqual(rustdoc_fail.returncode, 0, rustdoc_fail.stdout + rustdoc_fail.stderr)
        self.assertIn("FAILED crate-alpha (rustdoc", rustdoc_fail.stderr)
        self.assertIn("private_intra_doc_links", rustdoc_fail.stderr)

        # 6. Clean input in changed crate-alpha passes (even while unchanged crate-beta has debt).
        self.alpha.write_text(self.clean_alpha + "\n/// Returns two.\npub fn second() -> u32 {\n    2\n}\n")
        clean = self.run_make()
        self.assertEqual(clean.returncode, 0, clean.stdout + clean.stderr)


if __name__ == "__main__":
    unittest.main()

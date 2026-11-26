#!/usr/bin/env python3
"""Self-test for the Rust 1.89 parent-module import guard."""

import argparse
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent
CHECKER = SCRIPTS / "check-parent-module-imports.py"


def fixture_source(import_text):
    return f"""\
pub struct ParentType;
pub mod waiters {{
    pub struct ExclusiveWaiter;
}}
mod child {{
    {import_text}
}}
"""


REJECTED = {
    "direct_alias": fixture_source("use super as parent_alias;"),
    "visible_direct_alias": fixture_source("pub(crate) use super as parent_alias;"),
    "grouped_alias": fixture_source(
        "use super::{self as parent_alias, ParentType};"
    ),
    "visible_multiline_grouped_alias": fixture_source(
        "pub(super) use super::{\n        self as parent_alias,\n        ParentType,\n    };"
    ),
}

VALID = {
    "named_child_module_alias": fixture_source(
        "use super::waiters::{self as w};"
    ),
    "named_child_and_item": fixture_source(
        "use super::waiters::{self as admission_waiters, ExclusiveWaiter};"
    ),
    "aliased_parent_item": fixture_source("use super::{ParentType as Alias};"),
    "unaliased_parent_item": fixture_source("use super::ParentType;"),
}


class ParentModuleImportTests(unittest.TestCase):
    def run_checker(self, root):
        return subprocess.run(
            [sys.executable, str(CHECKER), str(root)],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_rejected_fixtures_name_file_line_and_replacement(self):
        for name, source in REJECTED.items():
            with self.subTest(fixture=name), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "src" / "lib.rs"
                path.parent.mkdir()
                path.write_text(source, encoding="utf-8")
                result = self.run_checker(Path(directory))
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("src/lib.rs:6:", result.stderr)
                self.assertIn("import the module by its full path", result.stderr)

    def test_valid_neighbours_pass(self):
        for name, source in VALID.items():
            with self.subTest(fixture=name), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "src" / "lib.rs"
                path.parent.mkdir()
                path.write_text(source, encoding="utf-8")
                result = self.run_checker(Path(directory))
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_comments_and_strings_do_not_look_like_imports(self):
        source = '''\
// use super as comment_alias;
/* outer comment /* use super as nested_alias; */ done */
const NOTE: &str = "use super::{self as string_alias, Item};";
pub struct Item;
'''
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "src" / "lib.rs"
            path.parent.mkdir()
            path.write_text(source, encoding="utf-8")
            result = self.run_checker(Path(directory))
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


def compiler_check(rustc):
    version = subprocess.run(
        [rustc, "--version"], capture_output=True, text=True, check=False
    )
    if version.returncode != 0:
        raise RuntimeError(version.stderr or version.stdout)
    if not version.stdout.startswith("rustc 1.89.0 "):
        raise RuntimeError(f"compiler must be Rust 1.89.0, found {version.stdout.strip()}")
    print(f"compiler: {rustc} --version -> {version.stdout.strip()}")
    cases = [(name, source, True) for name, source in REJECTED.items()]
    cases.extend((name, source, False) for name, source in VALID.items())
    with tempfile.TemporaryDirectory(prefix="parent-module-imports-") as directory:
        root = Path(directory)
        for name, source, expected_rejection in cases:
            case_dir = root / name
            case_dir.mkdir()
            source_path = case_dir / "lib.rs"
            source_path.write_text(source, encoding="utf-8")
            compiler = subprocess.run(
                [rustc, "--edition=2021", "--crate-type=lib", "--emit=metadata",
                 "--out-dir", str(case_dir), str(source_path)],
                capture_output=True,
                text=True,
                check=False,
            )
            checker = subprocess.run(
                [sys.executable, str(CHECKER), str(case_dir)],
                capture_output=True,
                text=True,
                check=False,
            )
            failed_as_expected = compiler.returncode != 0 and "error[E0432]" in compiler.stderr
            compiler_matches = failed_as_expected if expected_rejection else compiler.returncode == 0
            guard_matches = (checker.returncode == 1) if expected_rejection else checker.returncode == 0
            print(
                f"fixture {name}: rustc exit={compiler.returncode}, "
                f"E0432={'yes' if 'error[E0432]' in compiler.stderr else 'no'}, "
                f"guard exit={checker.returncode}"
            )
            if expected_rejection and not failed_as_expected:
                print(compiler.stderr, file=sys.stderr)
            if checker.returncode != 0:
                print(checker.stderr, file=sys.stderr)
            if not compiler_matches or not guard_matches:
                raise RuntimeError(f"compiler and guard verdict mismatch for {name}")
    print("Rust 1.89 compiler fixtures and guard verdicts passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--rustc",
        help="also compile every fixture with this Rust 1.89.0 compiler",
    )
    options = parser.parse_args()
    result = unittest.main(argv=[sys.argv[0]], exit=False)
    if not result.result.wasSuccessful():
        raise SystemExit(1)
    if options.rustc:
        try:
            compiler_check(options.rustc)
        except (OSError, RuntimeError) as error:
            print(f"compiler fixture check failed: {error}", file=sys.stderr)
            raise SystemExit(1) from error

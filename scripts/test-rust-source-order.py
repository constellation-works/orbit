#!/usr/bin/env python3
"""Exercise the Clippy and written source-order gates with offline Rust fixtures."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import unittest


ROOT = Path(__file__).resolve().parent.parent
HEADER = """\
//! Declaration-order fixture.

mod scoped_macro {
    #![expect(clippy::arbitrary_source_item_ordering, reason = "the child uses this macro")]

    macro_rules! number {
        () => { 1 };
    }

    mod child {
        pub const NUMBER: u32 = number!();
    }

    pub use child::NUMBER;
}

use scoped_macro::NUMBER;

macro_rules! default_second {
    () => { 2 };
}

"""
CONSTANT = "const FIRST: u32 = NUMBER;\n\n"
TYPE = """\
pub struct Pair {
    pub first: u32,
    pub second: u32,
}

"""
FUNCTIONS = """\
pub fn pair(first: u32, second: u32) -> Pair {
    Pair { first, second }
}

pub fn default_pair() -> Pair {
    pair(FIRST, default_second!())
}

"""
TESTS = """\
#[cfg(test)]
mod tests {
    #[test]
    fn ordered_fields() {
        assert_eq!(super::default_pair().first, 1);
    }
}
"""
CANONICAL = HEADER + CONSTANT + TYPE + FUNCTIONS + TESTS
OUT_OF_LINE_TEST = "#[cfg(test)]\nmod test_suite;\n\n"
INLINE_TEST = "#[cfg(test)]\nmod tests { #[test] fn exercise() {} }\n\n"
FUNCTION = "pub fn earlier() {}\n\n"
MODULE = "pub mod later {}\n\n"
LOCAL_TYPE = "pub struct Local;\n\n"
UNDERSCORE_CONST = "const _LATE: u32 = 0;\n\n"
CONDITIONAL_TYPE = (
    '#[cfg(any(target_os = "macos", target_os = "windows", feature = "alternate"))]\n'
    + LOCAL_TYPE
)
ORDERING = "clippy::arbitrary_source_item_ordering"
TESTS_LAST = "clippy::items_after_test_module"
LITERAL = "clippy::inconsistent_struct_constructor"


def create_order_fixture(repo):
    package = repo / "crates/demo"
    (package / "src").mkdir(parents=True)
    (package / "src/test_suite.rs").write_text("#[test]\nfn exercise() {}\n")
    (package / "Cargo.toml").write_text("""\
[workspace]
[package]
name = "order-probe"
version = "0.1.0"
edition = "2024"

[dependencies]
dependency = { path = "../../dependency" }

[features]
alternate = []

[lints.clippy]
arbitrary_source_item_ordering = "warn"
items_after_test_module = "warn"
inconsistent_struct_constructor = "warn"
""")
    (repo / "dependency/src").mkdir(parents=True)
    (repo / "dependency/Cargo.toml").write_text("""\
[package]
name = "dependency"
version = "0.1.0"
edition = "2024"
""")
    (repo / "dependency/src/lib.rs").write_text("""\
pub struct External { pub first: u32, pub second: u32 }

#[macro_export]
macro_rules! late_constant {
    () => { pub const GENERATED: u32 = 0; };
}
""")
    return package


class RustSourceOrderTests(unittest.TestCase):
    def test_pr_can_shrink_the_baseline_but_cannot_grow_it(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="rust-source-order-", dir=scratch) as directory:
            repo = Path(directory)
            scripts = repo / "scripts"
            scripts.mkdir()
            checker = scripts / "check-rust-source-order.py"
            shutil.copy2(ROOT / "scripts" / checker.name, checker)
            baseline = scripts / "rust-source-order-baseline.tsv"
            fingerprint = "a" * 24
            row = f"crates/demo/src/lib.rs\tliteral\t{fingerprint}\t"
            baseline.write_text(row + "2\n")
            for args in (["init", "-q"], ["add", "."],
                         ["-c", "user.name=Probe", "-c", "user.email=probe@example.com",
                          "commit", "-qm", "baseline"]):
                subprocess.run(["git", *args], cwd=repo, check=True)
            for count, status in ((1, 0), (2, 0), (3, 1)):
                with self.subTest(count=count):
                    baseline.write_text(row + f"{count}\n")
                    result = subprocess.run([checker, "--check-baseline"], cwd=repo,
                                            env=dict(os.environ, CI_TEST_BASE="HEAD"),
                                            capture_output=True, text=True, check=False)
                    self.assertEqual(result.returncode, status, result.stderr)
                    if status:
                        self.assertIn("added literal debt in crates/demo/src/lib.rs", result.stderr)

    def test_clippy_gate(self):
        ordering = "arbitrary_source_item_ordering"
        literal = "inconsistent_struct_constructor"
        cases = [
            ("canonical with a child macro exception", CANONICAL, None, None),
            ("type after a function", HEADER + CONSTANT + FUNCTIONS + TYPE + TESTS,
             ordering, "should be placed before `default_pair`"),
            ("constant after a type", HEADER + TYPE + CONSTANT + FUNCTIONS + TESTS,
             ordering, "should be placed before `Pair`"),
            ("test module before functions", HEADER + CONSTANT + TYPE + TESTS + FUNCTIONS,
             "items_after_test_module", "items after a test module"),
            ("reversed shorthand", CANONICAL.replace("Pair { first, second }", "Pair { second, first }"),
             literal, "struct constructor field order is inconsistent with struct definition field order"),
            ("reversed mixed literal", CANONICAL.replace(
                "Pair { first, second }", "Pair { second: second + 1, first }"),
             literal, "struct constructor field order is inconsistent with struct definition field order"),
            ("late import", CANONICAL.replace("use scoped_macro::NUMBER;\n\n", "").replace(
                TESTS, "use scoped_macro::NUMBER;\n\n" + TESTS),
             ordering, "incorrect ordering of items"),
            ("unrelated compiler warning", CANONICAL.replace(
                "    Pair { first, second }", "    let unused = 1;\n    Pair { first, second }"),
             "unused_variables", "unused variable: `unused`"),
        ]
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="rust-source-order-", dir=scratch) as directory:
            package = Path(directory)
            (package / "src").mkdir()
            manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
            lints = manifest["workspace"]["lints"]["clippy"]
            lint_lines = "\n".join(f'{name} = "{level}"' for name, level in lints.items())
            (package / "Cargo.toml").write_text(
                '[workspace]\n[package]\nname = "order-probe"\nversion = "0.1.0"\n'
                f'edition = "2024"\n[lints.clippy]\n{lint_lines}\n'
            )
            for name, source, lint, diagnostic in cases:
                with self.subTest(name=name):
                    (package / "src/lib.rs").write_text(source)
                    result = subprocess.run(
                        [ROOT / "scripts/with-test-build-lint.sh", "--check",
                         "cargo", "check", "--all-targets", "--message-format=json"],
                        cwd=package,
                        env=dict(os.environ, CLIPPY_CONF_DIR=str(ROOT),
                                 CARGO_TARGET_DIR=str(package / "target"), CARGO_NET_OFFLINE="true"),
                        capture_output=True, text=True, check=False,
                    )
                    self.assertEqual(result.returncode, int(lint is not None), result.stderr)
                    if lint:
                        self.assertIn(lint, result.stderr)
                        self.assertIn(diagnostic, result.stderr)
                        print(f"{name}: {lint}: {diagnostic}")
                    else:
                        print(f"{name}: passed")

    def test_clippy_coverage_and_complementary_source_order(self):
        """Exercise the Clippy 1.98 gaps and their coverage in the source gate."""
        cases = (
            ("out-of-line test before function", OUT_OF_LINE_TEST + FUNCTION, FUNCTION + OUT_OF_LINE_TEST,
             set(), "`fn earlier` must precede test module `mod test_suite`"),
            ("module after inline test", INLINE_TEST + MODULE, MODULE + INLINE_TEST,
             set(), "`mod later` must precede test module `mod tests`"),
            ("function after inline test", INLINE_TEST + FUNCTION, FUNCTION + INLINE_TEST,
             {TESTS_LAST}, "`fn earlier` must precede test module `mod tests`"),
            ("underscore constant after type", LOCAL_TYPE + UNDERSCORE_CONST, UNDERSCORE_CONST + LOCAL_TYPE,
             set(), "`const _LATE` must precede `struct Local`"),
            ("underscore function before type", FUNCTION.replace("pub fn earlier", "fn _helper") + LOCAL_TYPE,
             LOCAL_TYPE + FUNCTION.replace("pub fn earlier", "fn _helper"),
             set(), "`struct Local` must precede `fn _helper`"),
            ("inactive target and feature branch", FUNCTION + CONDITIONAL_TYPE, CONDITIONAL_TYPE + FUNCTION,
             set(), "`struct Local` must precede `fn earlier`"),
            ("external macro expansion", LOCAL_TYPE + "dependency::late_constant!();\n",
             None, set(), None),
            ("cross-crate struct literal",
             "pub fn build() -> dependency::External { dependency::External { second: 2, first: 1 } }\n",
             None, set(), None),
            ("local struct literal expansion",
             "macro_rules! reversed { () => { Local { second: 2, first: 1 } }; }\n\n"
             "pub struct Local { pub first: u32, pub second: u32 }\n\n"
             "pub fn build() -> Local { reversed!() }\n",
             None, set(), None),
            ("written local struct literal",
             "pub struct Local { pub first: u32, pub second: u32 }\n\n"
             "pub fn build() -> Local { Local { second: 2, first: 1 } }\n",
             None, {LITERAL}, None),
        )
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="rust-module-order-", dir=scratch) as directory:
            repo = Path(directory)
            package = create_order_fixture(repo)
            source = package / "src/lib.rs"
            guard = self.prepare_module_guard(repo)

            for name, bad, good, clippy_lints, finding in cases:
                with self.subTest(name=name):
                    source.write_text(bad)
                    self.assert_clippy_lints(package, clippy_lints)
                    if bad == FUNCTION + CONDITIONAL_TYPE:
                        self.assert_clippy_lints(package, {ORDERING}, "--features", "alternate")
                    result = guard()
                    self.assertEqual(result.returncode, int(finding is not None), result.stderr)
                    if finding:
                        self.assertIn("crates/demo/src/lib.rs:", result.stderr)
                        self.assertIn(f"STD-12 R20: {finding}", result.stderr)
                    if good:
                        source.write_text(good)
                        self.assert_clippy_lints(package, set())
                        result = guard()
                        self.assertEqual(result.returncode, 0, result.stderr)

    def assert_clippy_lints(self, package, expected, *flags):
        result = subprocess.run(
            ["cargo", "clippy", "--all-targets", "--message-format=json", *flags, "--", "-D", "warnings"],
            cwd=package, env=dict(os.environ, CLIPPY_CONF_DIR=str(ROOT),
                                  CARGO_TARGET_DIR=str(package.parents[1] / "target"), CARGO_NET_OFFLINE="true"),
            capture_output=True, text=True, check=False,
        )
        diagnostics = [row["message"] for row in map(json.loads, result.stdout.splitlines())
                       if row["reason"] == "compiler-message" and row["message"].get("code")]
        context = result.stderr + "\n" + "\n".join(item["rendered"] or "" for item in diagnostics)
        self.assertEqual(result.returncode, 101 if expected else 0, context)
        self.assertEqual({item["code"]["code"] for item in diagnostics}, expected, context)
        if expected:
            self.assertTrue(all(item["level"] == "error" for item in diagnostics), context)

    def prepare_module_guard(self, repo):
        scripts = repo / "scripts"
        scripts.mkdir()
        checker = scripts / "check-rust-module-order.py"
        for name in (checker.name, "check-windows-portability.py"):
            shutil.copy2(ROOT / "scripts" / name, scripts)
        self.git(repo, "init", "-q", "-b", "agent-main")
        self.commit(repo)
        base = self.git(repo, "rev-parse", "HEAD").stdout.strip()

        def guard():
            return subprocess.run([checker], cwd=repo, env=dict(os.environ, CI_TEST_BASE=base),
                                  capture_output=True, text=True, check=False)

        return guard

    def git(self, repo, *args):
        return subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True, check=True)

    def commit(self, repo):
        self.git(repo, "add", "--all")
        self.git(repo, "-c", "user.name=Probe", "-c", "user.email=probe@example.invalid",
                 "commit", "-qm", "fixture")

    def test_module_guard_keeps_scoped_expectations_and_strict_module_import_order(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="rust-module-order-", dir=scratch) as directory:
            repo = Path(directory)
            guard = self.prepare_module_guard(repo)
            source = repo / "desktop/src/lib.rs"
            source.parent.mkdir(parents=True)
            source.write_text(CANONICAL)
            result = guard()
            self.assertEqual(result.returncode, 0, result.stderr)

            expectation = ('#![expect(clippy::arbitrary_source_item_ordering, '
                           'reason = "the child uses make_value in lexical scope")]\n\n')
            code = """\
macro_rules! make_value {
    () => { 42 };
}

pub mod child {
    pub const VALUE: i32 = make_value!();
}

pub(crate) use make_value;

pub fn earlier() -> &'static str {
    r#"#[cfg(test)] mod fake; use unrelated::Name;"#
}
"""
            source.write_text(expectation + code)
            result = guard()
            self.assertEqual(result.returncode, 0, result.stderr)
            for late, diagnostic in (
                ("mod unrelated {}\n", "`mod unrelated` must precede `fn earlier`"),
                ("use std::{env, path::Path};\n", "`use std::{ env , path::Path }` must precede `fn earlier`"),
            ):
                with self.subTest(late=late):
                    source.write_text(expectation + code + "\n" + late)
                    result = guard()
                    self.assertEqual(result.returncode, 1, result.stderr)
                    self.assertIn(diagnostic, result.stderr)

            source.write_text(expectation.replace("clippy::", "") + code)
            result = guard()
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("`mod child` must precede `macro_rules make_value`", result.stderr)

            reexport = "pub(crate) use make_value;\n"
            source.write_text(expectation + code.replace(reexport, "") + "\n" + reexport)
            result = guard()
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("`use make_value` must precede `fn earlier`", result.stderr)

            source.write_text("pub mod outer {\n" + INLINE_TEST + MODULE + "}\n")
            result = guard()
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("desktop/src/lib.rs:5: STD-12 R20: `mod later`", result.stderr)

    def test_module_guard_compares_existing_debt_to_merge_base_across_renames(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="rust-module-order-", dir=scratch) as directory:
            repo = Path(directory)
            original = repo / "desktop/src/old.rs"
            original.parent.mkdir(parents=True)
            debt = FUNCTION + "#[cfg(all(test, unix))]\nmod test_suite;\n\n" + MODULE
            original.write_text(debt)
            guard = self.prepare_module_guard(repo)
            moved = original.with_name("new.rs")
            self.git(repo, "mv", original.relative_to(repo), moved.relative_to(repo))
            result = guard()
            self.assertEqual(result.returncode, 0, result.stderr)

            moved.write_text(debt + "use std::path::Path;\n")
            result = guard()
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("desktop/src/new.rs:8: STD-12 R20: `use std::path::Path`", result.stderr)
            self.assertNotIn("`mod later`", result.stderr)


if __name__ == "__main__":
    unittest.main()

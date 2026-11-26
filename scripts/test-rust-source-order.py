#!/usr/bin/env python3
"""Exercise the real Clippy source-order gate with dependency-free Rust."""

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


if __name__ == "__main__":
    unittest.main()

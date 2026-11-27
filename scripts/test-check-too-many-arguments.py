#!/usr/bin/env python3
"""Regression tests for scripts/check-too-many-arguments.py."""

import json
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
CHECKER = SCRIPTS / "check-too-many-arguments.py"


class TooManyArgumentsRatchetTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit" / "tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="too-many-args-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.baseline = self.root / "scripts" / "too-many-arguments-baseline.json"
        self.write("Cargo.toml", '[workspace]\nmembers = ["crates/alpha", "crates/beta"]\n')
        self.write("crates/alpha/Cargo.toml", '[package]\nname = "alpha"\nversion = "0.1.0"\n')
        self.write("crates/beta/Cargo.toml", '[package]\nname = "beta"\nversion = "0.1.0"\n')

    def write(self, relative, content):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(textwrap.dedent(content), encoding="utf-8")
        return path

    def run_checker(self, *args):
        return subprocess.run(
            [sys.executable, str(CHECKER), "--root", str(self.root), *args],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_counts_single_and_multiline_allow_and_expect_ignoring_comments_and_strings(self):
        self.write(
            "crates/alpha/src/lib.rs",
            """
            #[allow(clippy::too_many_arguments)]
            fn one() {}

            #[allow(
                clippy::too_many_arguments,
                reason = "legacy"
            )]
            fn two() {}

            #[expect(clippy::too_many_arguments, reason = "legacy")]
            fn three() {}

            #[expect(
                dead_code,
                clippy::too_many_arguments,
                reason = "combined and multi-line"
            )]
            fn four() {}

            #[cfg_attr(test, allow(clippy::too_many_arguments))]
            fn five() {}

            // #[allow(clippy::too_many_arguments)]
            /* outer /* #[expect(clippy::too_many_arguments)] */ */
            const DOC: &str = "#[allow(clippy::too_many_arguments)]";
            const RAW: &str = r#"#![allow(clippy::too_many_arguments)]"#;

            #[warn(clippy::too_many_arguments)]
            #[deny(clippy::too_many_arguments)]
            fn allowed_or_denied_not_counted() {}
            """,
        )
        self.write("crates/beta/src/lib.rs", "pub fn clean() {}\n")

        written = self.run_checker("--write-baseline")
        self.assertEqual(written.returncode, 0, written.stderr)
        self.assertEqual(
            json.loads(self.baseline.read_text(encoding="utf-8")),
            {"schema_version": 1, "crates": {"alpha": 5}},
        )
        checked = self.run_checker("--list")
        self.assertEqual(checked.returncode, 0, checked.stderr)
        self.assertIn("alpha: 5", checked.stdout)
        self.assertIn("5 attributes across 1 crates match the baseline", checked.stdout)

    def test_counts_lint_groups_that_cover_too_many_arguments(self):
        self.write(
            "crates/alpha/src/lib.rs",
            """
            #[allow(clippy::complexity)]
            fn one() {}

            #[expect(clippy::all, reason = "legacy")]
            fn two() {}

            #[allow(
                warnings,
                reason = "multi-line"
            )]
            fn three() {}

            #[cfg_attr(not(test), allow(clippy::complexity))]
            fn four() {}

            #[allow(clippy::too_many_arguments, clippy::complexity)]
            fn five() {}
            """,
        )
        self.write("crates/beta/src/lib.rs", "pub fn clean() {}\n")

        self.assertEqual(self.run_checker("--write-baseline").returncode, 0)
        self.assertEqual(
            json.loads(self.baseline.read_text(encoding="utf-8")),
            {"schema_version": 1, "crates": {"alpha": 5}},
        )
        checked = self.run_checker("--list")
        self.assertEqual(checked.returncode, 0, checked.stderr)
        for directive in ("allow(clippy::complexity)", "expect(clippy::all)", "allow(warnings)"):
            with self.subTest(directive=directive):
                self.assertIn(directive, checked.stdout)

        # A group allow in a crate with baseline 0 is a rise, named by its own lint.
        self.write("crates/beta/src/lib.rs", "#[allow(clippy::complexity)]\nfn added() {}\n")
        rise = self.run_checker()
        self.assertEqual(rise.returncode, 1)
        self.assertIn("beta has 1 allow/expect attribute(s)", rise.stderr)
        self.assertIn("crates/beta/src/lib.rs:1 #[allow(clippy::complexity)]", rise.stderr)

    def test_ignores_unrelated_lint_attributes_and_non_allowing_directives(self):
        self.write(
            "crates/alpha/src/lib.rs",
            """
            #[allow(dead_code)]
            #[allow(clippy::style, clippy::pedantic, clippy::too_many_lines)]
            #[allow(unused_variables, reason = "legacy")]
            fn one() {}

            #[warn(clippy::complexity)]
            #[deny(clippy::all, warnings)]
            #[forbid(clippy::complexity)]
            fn two() {}

            #[cfg_attr(test, deny(clippy::complexity))]
            fn three() {}

            // #[allow(clippy::complexity)]
            /* outer /* #![allow(warnings)] */ */
            const DOC: &str = "#[allow(clippy::all)]";
            """,
        )
        self.write("crates/beta/src/lib.rs", "pub fn clean() {}\n")

        self.assertEqual(self.run_checker("--write-baseline").returncode, 0)
        self.assertEqual(json.loads(self.baseline.read_text(encoding="utf-8"))["crates"], {})
        self.assertEqual(self.run_checker().returncode, 0)

    def test_fails_when_crate_count_rises_or_new_crate_adds_attribute(self):
        self.write("crates/alpha/src/lib.rs", "#[allow(clippy::too_many_arguments)]\nfn one() {}\n")
        self.write("crates/beta/src/lib.rs", "pub fn clean() {}\n")
        self.assertEqual(self.run_checker("--write-baseline").returncode, 0)

        # Adding an attribute in alpha rises 1 -> 2.
        self.write(
            "crates/alpha/src/lib.rs",
            "#[allow(clippy::too_many_arguments)]\nfn one() {}\n"
            "#[expect(clippy::too_many_arguments, reason = \"x\")]\nfn two() {}\n",
        )
        rise = self.run_checker()
        self.assertEqual(rise.returncode, 1)
        self.assertIn("alpha has 2 allow/expect attribute(s)", rise.stderr)
        self.assertIn("above its baseline of 1", rise.stderr)
        self.assertIn("crates/alpha/src/lib.rs:3", rise.stderr)

        # --write-baseline refuses to raise an existing count.
        refused = self.run_checker("--write-baseline")
        self.assertEqual(refused.returncode, 1)
        self.assertIn("refusing to raise the baseline", refused.stderr)

        # Restore alpha, then add an attribute to previously clean beta (baseline 0).
        self.write("crates/alpha/src/lib.rs", "#[allow(clippy::too_many_arguments)]\nfn one() {}\n")
        self.write("crates/beta/src/lib.rs", "#[allow(clippy::too_many_arguments)]\nfn added() {}\n")
        beta_rise = self.run_checker()
        self.assertEqual(beta_rise.returncode, 1)
        self.assertIn("beta has 1 allow/expect attribute(s)", beta_rise.stderr)
        self.assertIn("above its baseline of 0", beta_rise.stderr)
        self.assertEqual(self.run_checker("--write-baseline").returncode, 1)

    def test_fails_when_below_baseline_until_baseline_is_lowered(self):
        self.write(
            "crates/alpha/src/lib.rs",
            "#[allow(clippy::too_many_arguments)]\nfn one() {}\n"
            "#[allow(clippy::too_many_arguments)]\nfn two() {}\n",
        )
        self.write("crates/beta/src/lib.rs", "pub fn clean() {}\n")
        self.assertEqual(self.run_checker("--write-baseline").returncode, 0)

        # Removing one attribute makes count 1 < baseline 2 -> must fail.
        self.write("crates/alpha/src/lib.rs", "#[allow(clippy::too_many_arguments)]\nfn one() {}\n")
        below = self.run_checker()
        self.assertEqual(below.returncode, 1)
        self.assertIn("alpha has 1 allow/expect attribute(s)", below.stderr)
        self.assertIn("below its baseline of 2", below.stderr)
        self.assertIn("--write-baseline", below.stderr)

        # Lowering the baseline resolves the failure.
        lowered = self.run_checker("--write-baseline")
        self.assertEqual(lowered.returncode, 0, lowered.stderr)
        self.assertEqual(json.loads(self.baseline.read_text(encoding="utf-8"))["crates"], {"alpha": 1})
        self.assertEqual(self.run_checker().returncode, 0)

        # Removing the last attribute also fails until lowered to empty.
        self.write("crates/alpha/src/lib.rs", "pub fn one() {}\n")
        self.assertEqual(self.run_checker().returncode, 1)
        self.assertEqual(self.run_checker("--write-baseline").returncode, 0)
        self.assertEqual(json.loads(self.baseline.read_text(encoding="utf-8"))["crates"], {})
        self.assertEqual(self.run_checker().returncode, 0)

    def test_fails_on_inner_attributes(self):
        self.write("crates/alpha/src/lib.rs", "pub fn clean() {}\n")
        self.assertEqual(self.run_checker("--write-baseline").returncode, 0)

        for snippet in (
            "#![allow(clippy::too_many_arguments)]\n",
            "#![expect(clippy::too_many_arguments, reason = \"crate-wide\")]\n",
            "#![allow(\n    clippy::too_many_arguments,\n    reason = \"multi-line\"\n)]\n",
            "#![cfg_attr(test, allow(clippy::too_many_arguments))]\n",
            "#![allow(clippy::complexity)]\n",
            "#![expect(clippy::all, reason = \"crate-wide\")]\n",
            "#![allow(warnings)]\n",
            "#![cfg_attr(not(test), allow(clippy::complexity))]\n",
        ):
            with self.subTest(snippet=snippet):
                self.write("crates/alpha/src/lib.rs", snippet)
                result = self.run_checker()
                self.assertEqual(result.returncode, 1)
                self.assertIn("forbidden inner", result.stderr)
                self.assertIn("crates/alpha/src/lib.rs:1", result.stderr)
                self.assertEqual(self.run_checker("--write-baseline").returncode, 1)

        # Inner deny/warn attributes, including group forms, are permitted.
        self.write("crates/alpha/src/lib.rs", "#![deny(clippy::too_many_arguments)]\n")
        self.assertEqual(self.run_checker().returncode, 0)
        self.write("crates/alpha/src/lib.rs", "#![deny(clippy::complexity, warnings)]\n#![warn(clippy::all)]\n")
        self.assertEqual(self.run_checker().returncode, 0)

    def test_fails_on_manifest_lint_allows(self):
        self.write("crates/alpha/src/lib.rs", "pub fn clean() {}\n")
        self.assertEqual(self.run_checker("--write-baseline").returncode, 0)

        cases = (
            ("Cargo.toml", '[workspace]\nmembers = ["crates/alpha"]\n[workspace.lints.clippy]\ntoo_many_arguments = "allow"\n'),
            ("Cargo.toml", '[workspace]\nmembers = ["crates/alpha"]\n[workspace.lints.clippy]\ntoo-many-arguments = { level = "allow", priority = -1 }\n'),
            ("Cargo.toml", '[workspace]\nmembers = ["crates/alpha"]\n[workspace.lints.clippy]\ncomplexity = "allow"\n'),
            ("crates/alpha/Cargo.toml", '[package]\nname = "alpha"\nversion = "0.1.0"\n[lints.clippy]\ntoo_many_arguments = "allow"\n'),
            ("desktop/Cargo.toml", '[package]\nname = "orbit-desktop"\nversion = "0.1.0"\n[lints.clippy]\ntoo_many_arguments = { level = "allow" }\n'),
        )
        for manifest, content in cases:
            with self.subTest(manifest=manifest, content=content):
                self.write("Cargo.toml", '[workspace]\nmembers = ["crates/alpha"]\n')
                self.write("crates/alpha/Cargo.toml", '[package]\nname = "alpha"\nversion = "0.1.0"\n')
                desktop_manifest = self.root / "desktop/Cargo.toml"
                if desktop_manifest.exists():
                    desktop_manifest.unlink()
                self.write(manifest, content)
                result = self.run_checker()
                self.assertEqual(result.returncode, 1)
                self.assertIn(manifest, result.stderr)
                self.assertIn('= "allow" is forbidden', result.stderr)
                self.assertEqual(self.run_checker("--write-baseline").returncode, 1)

        # Warn or deny in manifest is permitted.
        self.write(
            "Cargo.toml",
            '[workspace]\nmembers = ["crates/alpha"]\n[workspace.lints.clippy]\ntoo_many_arguments = "warn"\n',
        )
        self.write("crates/alpha/Cargo.toml", '[package]\nname = "alpha"\nversion = "0.1.0"\n[lints]\nworkspace = true\n')
        (self.root / "desktop/Cargo.toml").unlink(missing_ok=True)
        self.assertEqual(self.run_checker().returncode, 0)

    def test_missing_or_malformed_baseline_exits_two(self):
        self.write("crates/alpha/src/lib.rs", "pub fn clean() {}\n")
        missing = self.run_checker()
        self.assertEqual(missing.returncode, 2)
        self.assertIn("cannot read baseline", missing.stderr)
        self.assertNotIn("Traceback", missing.stderr)

        for bad in ("{}", '{"schema_version": 99, "crates": {}}', '{"schema_version": 1, "crates": {"a": -1}}'):
            with self.subTest(bad=bad):
                self.write("scripts/too-many-arguments-baseline.json", bad)
                result = self.run_checker()
                self.assertEqual(result.returncode, 2)
                self.assertNotIn("Traceback", result.stderr)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Run the Linux-gated test item guard against small Rust test trees."""

from pathlib import Path
import self_test_hang
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent
LINUX = '#[cfg(target_os = "linux")]'
MANIFEST = '[package]\nname = "example"\nversion = "0.1.0"\n'
SUITE = "crates/example/tests/suite.rs"
SUPPORT = "crates/example/tests/support/mod.rs"


def linux_test(body):
    """A Linux-only test whose body is `body`, so it is the sole user of what the body names."""
    return f"{LINUX}\n#[test]\nfn linux_only() {{ {body} }}\n"


class LinuxGatedTestItemTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="linux-gated-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def check_tree(self, files, expected, *messages):
        # Each invocation is a fresh tree, so files from another fixture cannot
        # change module reachability.
        with tempfile.TemporaryDirectory(dir=self.root) as directory:
            root = Path(directory)
            files = {"crates/example/Cargo.toml": MANIFEST, **files}
            for name, source in files.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source)
            result = self_test_hang.run(
                [sys.executable, str(SCRIPTS / "check-linux-gated-test-items.py"), "--root", str(root)],
                step="linux-gated test items",
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
            for message in messages:
                self.assertIn(message, result.stderr)
            return result

    def test_ungated_import_used_only_by_linux_tests_is_reported_with_file_line_and_item(self):
        source = "use std::fs;\n\n" + linux_test('fs::read("x").unwrap();')
        self.check_tree({SUITE: source}, 1, "crates/example/tests/suite.rs:1:", "import `fs`")
        self.check_tree({SUITE: f"{LINUX}\n" + source}, 0)
        self.check_tree({SUITE: "#![cfg(target_os = \"linux\")]\n" + source}, 0)

    def test_ungated_grouped_and_aliased_imports_are_reported_per_binding(self):
        source = "use std::{fs, io::{self as stream}};\n\n" + linux_test("fs::read(\"x\"); stream::empty();")
        result = self.check_tree({SUITE: source}, 1, "import `fs`", "import `stream`")
        self.assertNotIn("import `io`", result.stderr)

    def test_ungated_helper_function_used_only_by_linux_tests_is_reported(self):
        source = "fn fixture() -> u32 {\n    7\n}\n\n" + linux_test("assert_eq!(fixture(), 7);")
        self.check_tree({SUITE: source}, 1, "crates/example/tests/suite.rs:1:", "test helper fn `fixture`")
        self.check_tree({SUITE: f"{LINUX}\n" + source}, 0)
        for keyword, item in (("const", "const LIMIT: u32 = 7;"), ("static", "static LIMIT: u32 = 7;")):
            with self.subTest(keyword=keyword):
                source = item + "\n\n" + linux_test("assert_eq!(LIMIT, 7);")
                self.check_tree({SUITE: source}, 1, f"test helper {keyword} `LIMIT`")
                self.check_tree({SUITE: f"{LINUX}\n" + source}, 0)

    def test_ungated_fixture_module_used_only_by_linux_tests_is_reported(self):
        files = {SUITE: "mod support;\n\n" + linux_test("support::command();"),
                 SUPPORT: "pub fn command() {}\n"}
        self.check_tree(files, 1, "crates/example/tests/suite.rs:1:", "fixture module `support`")
        files[SUITE] = f"{LINUX}\n" + files[SUITE]
        self.check_tree(files, 0)
        # A module declared inside a Linux-only module is Linux-only too.
        self.check_tree({SUITE: f"{LINUX}\nmod linux;",
                         "crates/example/tests/linux/mod.rs": "mod support;\n" + linux_test("support::command();"),
                         "crates/example/tests/linux/support.rs": "pub fn command() {}\n"}, 0)

    def test_fixture_module_reached_through_path_and_crate_paths_is_reported(self):
        files = {SUITE: '#[path = "helpers/git.rs"]\nmod git_support;\nmod linux;\n',
                 "crates/example/tests/helpers/git.rs": "pub fn command() {}\n",
                 "crates/example/tests/linux.rs": "#![cfg(target_os = \"linux\")]\n"
                 "#[test]\nfn t() { crate::git_support::command(); }\n"}
        self.check_tree(files, 1, "crates/example/tests/suite.rs:2:", "fixture module `git_support`")
        files[SUITE] = f"{LINUX}\n" + files[SUITE].replace("mod linux;", f"{LINUX}\nmod linux;")
        self.check_tree(files, 0)

    def test_glob_import_with_no_code_compiled_on_macos_is_reported(self):
        files = {SUITE: "mod probes;\n",
                 "crates/example/tests/suite/probes.rs": "use super::*;\n\n" + linux_test("helper();")}
        self.check_tree(files, 1, "suite/probes.rs:1:", "glob import `super::*`")
        files["crates/example/tests/suite/probes.rs"] += "\n#[test]\nfn everywhere() {}\n"
        self.check_tree(files, 0)
        files["crates/example/tests/suite/probes.rs"] = f"{LINUX}\nuse super::*;\n\n" + linux_test("helper();")
        self.check_tree(files, 0)

    def test_a_use_that_macos_compiles_keeps_the_item_valid(self):
        for gate in ("", "#[cfg(unix)]", '#[cfg(target_os = "macos")]', "#[cfg(not(windows))]",
                     '#[cfg(any(target_os = "linux", target_os = "macos"))]',
                     '#[cfg(not(target_os = "linux"))]', '#[cfg(feature = "slow")]'):
            with self.subTest(gate=gate):
                source = ("use std::fs;\nfn fixture() {}\n\n" + linux_test("fs::read(\"x\"); fixture();")
                          + f"\n{gate}\n#[test]\nfn portable() {{ fs::read(\"y\"); fixture(); }}\n")
                self.check_tree({SUITE: source}, 0)

    def test_inline_format_arguments_and_attribute_arguments_are_uses(self):
        live = '#[test]\nfn portable() { println!("{LIMIT}"); }\n'
        self.check_tree({SUITE: "const LIMIT: u32 = 7;\n" + linux_test("") + live}, 0)
        self.check_tree({SUITE: "const LIMIT: u32 = 7;\n" + linux_test('println!("{LIMIT:?}");')}, 1)
        self.check_tree({SUITE: "use serde::Serialize;\n#[derive(Serialize)]\nstruct Row;\n"
                         "use std::fs;\n" + linux_test("fs::read(\"x\");")}, 1, "import `fs`")

    def test_raw_string_format_captures_are_uses_and_linux_only_captures_are_reported(self):
        for literal in ('r#"{LIMIT}"#', 'r##"{LIMIT}"##'):
            with self.subTest(literal=literal):
                portable = '#[test]\nfn portable() { assert_eq!(format!(' + literal + '), "7"); }\n'
                self.check_tree({SUITE: "const LIMIT: u32 = 7;\n"
                                 + linux_test("assert_eq!(LIMIT, 7);") + portable}, 0)

        self.check_tree({SUITE: 'const LIMIT: u32 = 7;\n' + linux_test('println!(r#"{LIMIT}"#);')},
                        1, "test helper const")
        self.check_tree({SUITE: 'const LIMIT: u32 = 7;\n' + linux_test("assert_eq!(LIMIT, 7);")
                         + '#[test]\nfn portable() { let _ = r#"LIMIT"#; }\n'},
                        1, "test helper const")

    def test_cfg_test_modules_in_source_files_are_test_code(self):
        body = "    use std::fs;\n\n    " + linux_test('fs::read("x");').replace("\n", "\n    ")
        source = "pub fn run() {}\n\n#[cfg(test)]\nmod tests {\n" + body + "}\n"
        self.check_tree({"crates/example/src/lib.rs": source}, 1, "src/lib.rs:5:", "import `fs`")
        # Production code is not test scope.
        self.check_tree({"crates/example/src/lib.rs": "use std::fs;\n\n" + linux_test('fs::read("x");')}, 0)
        self.check_tree({"crates/example/src/lib.rs": "#[cfg(test)]\nmod tests;\n",
                         "crates/example/src/tests.rs": "use std::fs;\n" + linux_test('fs::read("x");')},
                        1, "src/tests.rs:1:")

    def test_desktop_test_files_are_checked(self):
        self.check_tree({"desktop/Cargo.toml": MANIFEST,
                         "desktop/tests/update.rs": "fn fixture() {}\n" + linux_test("fixture();")},
                        1, "desktop/tests/update.rs:1:", "test helper fn `fixture`")

    def test_items_the_guard_cannot_judge_are_left_alone(self):
        cases = (
            "#[allow(dead_code)]\nfn fixture() {}\n" + linux_test("fixture();"),
            "#![allow(unused_imports)]\nuse std::fs;\n" + linux_test('fs::read("x");'),
            "pub fn fixture() {}\n" + linux_test("fixture();"),
            "use std::io::Write;\n" + linux_test("std::io::stdout().flush();"),
            "fn fixture() {}\n" + linux_test("x.fixture(); other::fixture();"),
            "fn unused() {}\n",
            "use std::fs;\n",
            "#[test]\nfn only_linux_runs_this_name() {}\n",
        )
        for source in cases:
            with self.subTest(source=source):
                self.check_tree({SUITE: source}, 0)

    def test_module_holding_tests_or_gated_by_inner_cfg_is_not_a_fixture(self):
        files = {SUITE: "mod cases;\nmod linux_cases;\n" + linux_test("cases::helper(); linux_cases::helper();"),
                 "crates/example/tests/cases.rs": "pub fn helper() {}\n#[test]\nfn runs() {}\n",
                 "crates/example/tests/linux_cases.rs": '#![cfg(target_os = "linux")]\npub fn helper() {}\n'}
        self.check_tree(files, 0)

    def test_included_files_share_their_includers_scope(self):
        files = {SUITE: 'use std::fs;\ninclude!("cases.rs");\n',
                 "crates/example/tests/cases.rs": "#[test]\nfn reads() { fs::read(\"x\"); }\n"}
        self.check_tree(files, 0)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Run the portability guard against small Rust module trees."""

from pathlib import Path
import self_test_hang
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent
IMPORT = "use std::os::unix::fs::PermissionsExt;"
INCIDENTS = (
    '#[test]\nfn replay() {\n    ' + IMPORT + '\n    std::fs::Permissions::from_mode(0o755);\n}',
    'fn provider_reports_pr_17() {\n    ' + IMPORT + '\n    std::fs::Permissions::from_mode(0o755);\n}',
)


class PortabilityTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="windows-portability-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def check_tree(self, files, expected):
        # Each invocation is a fresh tree: absent files must not affect module
        # reachability from another fixture in the same test.
        with tempfile.TemporaryDirectory(dir=self.root) as directory:
            root = Path(directory)
            for name, source in files.items():
                path = root / "crates/example" / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source)
            result = self_test_hang.run(
                [sys.executable, str(SCRIPTS / "check-windows-portability.py"), "--root", str(root)],
                step="windows portability",
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
            if expected:
                self.assertIn("crates/example/", result.stderr)
            return result

    def test_both_windows_compile_incidents_require_a_gate(self):
        for source in INCIDENTS:
            with self.subTest(source=source):
                result = self.check_tree({"src/lib.rs": source}, 1)
                self.assertIn("src/lib.rs:3:", result.stderr)
                self.check_tree({"src/lib.rs": "#[cfg(unix)]\n" + source}, 0)
                self.check_tree({"src/lib.rs": "#![cfg(unix)]\n" + source}, 0)
                self.check_tree({"src/lib.rs": "#[cfg(unix)] mod tests {" + source + "}"}, 0)

    def test_external_module_gates_propagate_through_both_layouts(self):
        for child in ("src/helper.rs", "src/helper/mod.rs"):
            for gate in ("#[cfg(unix)]", '#[cfg(target_os = "linux")]', "#[cfg(not(windows))]"):
                with self.subTest(child=child, gate=gate):
                    files = {"src/lib.rs": gate + " mod helper;", child: INCIDENTS[1]}
                    self.check_tree(files, 0)
                    files["src/lib.rs"] = "mod helper;"
                    self.check_tree(files, 1)

    def test_nested_and_path_modules_and_file_gates(self):
        trees = (
            {"src/lib.rs": "#[cfg(unix)] mod platform;", "src/platform.rs": "mod helper;",
             "src/platform/helper.rs": INCIDENTS[1]},
            {"src/lib.rs": "mod platform;", "src/platform.rs": "#![cfg(unix)]\nmod helper;",
             "src/platform/helper.rs": INCIDENTS[1]},
            {"src/lib.rs": '#[cfg(unix)] #[path = "helpers/provider.rs"] mod helper;',
             "src/helpers/provider.rs": INCIDENTS[1]},
            {"src/lib.rs": "#[cfg(unix)] mod platform { mod helper; }",
             "src/platform/helper.rs": INCIDENTS[1]},
            {"src/lib.rs": 'mod platform { #[cfg(unix)] #[path = "custom.rs"] mod helper; }',
             "src/platform/custom.rs": INCIDENTS[1]},
        )
        for tree in trees:
            with self.subTest(tree=tree):
                self.check_tree(tree, 0)

    def test_cargo_test_and_custom_library_roots_resolve_sibling_modules(self):
        for root in ("tests/suite.rs", "src/custom.rs"):
            with self.subTest(root=root):
                manifest = '[package]\nname = "fixture"\nversion = "0.1.0"\n'
                if root.startswith("src/"):
                    manifest += '[lib]\npath = "src/custom.rs"\n'
                helper = str(Path(root).parent / "support/helper.rs")
                files = {"Cargo.toml": manifest, root: "#[cfg(unix)] mod support;",
                         str(Path(root).parent / "support/mod.rs"): "mod helper;",
                         helper: INCIDENTS[1]}
                self.check_tree(files, 0)
                files[root] = "mod support;"
                self.check_tree(files, 1)

    def test_one_ungated_importing_module_or_cargo_target_still_fails(self):
        self.check_tree({
            "src/lib.rs": '#[cfg(unix)] #[path = "helper.rs"] mod a;\n#[path = "helper.rs"] mod b;',
            "src/helper.rs": INCIDENTS[1],
        }, 1)
        self.check_tree({
            "Cargo.toml": '[package]\nname = "fixture"\nversion = "0.1.0"\n',
            "src/lib.rs": '#[cfg(unix)] #[path = "../tests/helper.rs"] mod helper;',
            "tests/helper.rs": INCIDENTS[1],
        }, 1)
        self.check_tree({
            "Cargo.toml": '[package]\nname = "fixture"\nversion = "0.1.0"\n'
                          '[[test]]\nname = "helper"\npath = "fixtures/helper.rs"\n',
            "src/lib.rs": '#[cfg(unix)] #[path = "../fixtures/helper.rs"] mod helper;',
            "fixtures/helper.rs": INCIDENTS[1],
        }, 1)
        for kind, folder in (("tests", "tests"), ("benches", "benches")):
            with self.subTest(disabled_auto_target=kind):
                self.check_tree({
                    "Cargo.toml": '[package]\nname = "fixture"\nversion = "0.1.0"\n'
                                  f'auto{kind} = false\n',
                    "src/lib.rs": f'#[cfg(unix)] #[path = "../{folder}/helper.rs"] mod helper;',
                    f"{folder}/helper.rs": INCIDENTS[1],
                }, 0)

    def test_cfg_logic_and_cfg_attr_do_not_confuse_runtime_conditions(self):
        allowed = (
            "unix", "not(windows)", 'target_os = "macos"', 'target_family = "unix"',
            'any(target_os = "linux", target_os = "macos")',
            'all(test, unix)', 'all(feature = "fixture", not(windows))',
            'not(any(windows, feature = "fixture"))',
        )
        refused = (
            "test", 'feature = "fixture"', "windows", "any(unix, test)",
            'any(unix, target_os = "windows")', "not(unix)",
        )
        for condition in allowed + refused:
            with self.subTest(condition=condition):
                self.check_tree({"src/lib.rs": f"#[cfg({condition})]\n" + INCIDENTS[1]},
                                0 if condition in allowed else 1)
        for attr, expected in (
            ("#[cfg_attr(windows, cfg(unix))]", 0),
            ("#[cfg_attr(not(unix), cfg(not(windows)))]", 0),
            ("#[cfg_attr(unix, cfg(unix))]", 1),
            ('#[cfg_attr(feature = "fixture", cfg(unix))]', 1),
        ):
            with self.subTest(attr=attr):
                self.check_tree({"src/lib.rs": attr + "\n" + INCIDENTS[1]}, expected)
        self.check_tree({"src/lib.rs": "fn helper() { if cfg!(unix) {" + IMPORT + "} }"}, 1)

    def test_gates_do_not_leak_to_sibling_items_or_imports(self):
        self.check_tree({"src/lib.rs": "#[cfg(unix)] fn gated() {}\n" + INCIDENTS[1]}, 1)
        self.check_tree({"src/lib.rs": "#[cfg(unix)] " + IMPORT + "\n" + INCIDENTS[1]}, 1)
        self.check_tree({"src/lib.rs": "#[cfg(unix)] mod gated {}\n" + INCIDENTS[1]}, 1)
        self.check_tree({"src/lib.rs": "fn helper() { #[cfg(unix)] {" + IMPORT + "} }"}, 0)
        for source in (
            "struct Data { #[cfg(unix)] platform: (), on_windows: std::os::unix::net::UnixStream }",
            "struct Data(#[cfg(unix)] (), std::os::unix::net::UnixStream);",
            "enum Data { #[cfg(unix)] Platform, OnWindows(std::os::unix::net::UnixStream) }",
        ):
            with self.subTest(source=source):
                self.check_tree({"src/lib.rs": source}, 1)
                self.check_tree({"src/lib.rs": "#[cfg(unix)] " + source}, 0)

    def test_grouped_aliased_and_fully_qualified_imports_and_calls(self):
        sources = (
            "use std::os::{unix::fs::PermissionsExt, windows::fs::MetadataExt};",
            "use std::{os::{unix::fs::PermissionsExt as Ext}};",
            "use ::std::os::unix::process::CommandExt as Ext;",
            "fn helper() { ::std::os::unix::fs::symlink(a, b); }",
            "fn helper() { std::fs::Permissions::from_mode(0o755); }",
            "fn helper() { command.pre_exec(|| Ok(())); }",
            "fn helper() { metadata.ino(); metadata.uid(); }",
            "fn helper() { MetadataExt::blocks(&metadata); }",
            "fn helper() { fs::metadata(path)?.permissions().mode(); }",
        )
        for source in sources:
            with self.subTest(source=source):
                self.check_tree({"src/lib.rs": source}, 1)
                self.check_tree({"src/lib.rs": "#[cfg(unix)]\n" + source}, 0)

    def test_comments_literals_lifetimes_and_application_methods_are_ignored(self):
        self.check_tree({"src/lib.rs": r'''// use std::os::unix::fs::PermissionsExt;
/* nested /* comment */ std::os::unix::fs::symlink(a, b); */
fn helper<'a>(value: &'a str) {
    let a = "std::os::unix::fs::PermissionsExt; }";
    let b = br###"#[cfg(unix)] { command.pre_exec(); }"###;
    let c = b'}'; let d = '\'';
    output.mode(); failure.blocks(); header.set_mode(0o755);
    std::os::windows::fs::MetadataExt::file_attributes(&metadata);
}
'''}, 0)

    def test_windows_looking_or_unix_looking_filenames_do_not_exempt_code(self):
        self.check_tree({"src/unix.rs": INCIDENTS[1]}, 1)


if __name__ == "__main__":
    unittest.main()

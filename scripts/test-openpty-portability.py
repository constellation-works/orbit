#!/usr/bin/env python3
"""Exercise the openpty portability guard through its command interface."""

from pathlib import Path
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))
import self_test_hang


class OpenptyPortabilityTests(unittest.TestCase):
    def check_tree(self, files, expected):
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="openpty-portability-", dir=scratch) as directory:
            root = Path(directory)
            for name, content in files.items():
                path = root / "crates/example/src" / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content)
            result = self_test_hang.run(
                [sys.executable, str(SCRIPTS / "check-openpty-portability.py"), "--root", str(root)],
                step="openpty portability", capture_output=True, text=True)
            self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
            return result

    def test_incident_shapes_and_repairs(self):
        for bad, good in (("std::ptr::null()", "std::ptr::null_mut()"),
                          ("&dimensions", "std::ptr::addr_of_mut!(dimensions)"),
                          ("&requested", "std::ptr::addr_of_mut!(requested)")):
            for position in (3, 4):
                with self.subTest(bad=bad, position=position):
                    args = ["a", "b", "c", "std::ptr::null_mut()", "std::ptr::null_mut()"]
                    args[position] = bad
                    source = "fn fixture() {\n libc::openpty(\n " + ",\n ".join(args) + ",\n );\n}"
                    result = self.check_tree({"lib.rs": source}, 1)
                    self.assertIn(f"crates/example/src/lib.rs:{position + 3}: immutable openpty argument: {bad}", result.stderr)
                    self.check_tree({"lib.rs": source.replace(bad, good)}, 0)

    def test_lexical_forms_and_literals(self):
        for bad in ("core::ptr::null()", "ptr::null()", "ptr::addr_of!(value)",
                    "core::ptr::addr_of!(value)", "value as *const libc::winsize", "(&value)"):
            with self.subTest(bad=bad):
                result = self.check_tree({"lib.rs": f"fn f() {{ libc::openpty(a,b,c,d,{bad}); }}"}, 1)
                self.assertIn(bad, result.stderr)
        self.check_tree({"lib.rs": r'''fn f() {
            // libc::openpty(a,b,c,d,&bad);
            let s = "libc::openpty(a,b,c,d,&bad)";
            let s = r#"libc::openpty(a,b,c,d,ptr::null())"#;
            libc::openpty(a, b, c, /* comment */ &mut term, &raw mut size);
            libc::openpty(a,b,c,ptr::null_mut(), value as *mut libc::winsize);
            libc::openpty(a,b,c,ptr::addr_of_mut!(term), (&size as *const _ as *mut _));
        }'''}, 0)

    def test_leading_absolute_paths(self):
        immutable = (
            "::std::ptr::null()",
            "(::std::ptr::null())",
            "::core::ptr::null()",
            "(::core::ptr::null())",
            "::std::ptr::addr_of!(value)",
            "(::std::ptr::addr_of!(value))",
        )
        for bad in immutable:
            for position in (3, 4):
                with self.subTest(bad=bad, position=position):
                    args = ["a", "b", "c", "::std::ptr::null_mut()", "::std::ptr::null_mut()"]
                    args[position] = bad
                    source = "fn fixture() {\n libc::openpty(\n " + ",\n ".join(args) + ",\n );\n}"
                    result = self.check_tree({"lib.rs": source}, 1)
                    self.assertIn(
                        f"crates/example/src/lib.rs:{position + 3}: immutable openpty argument: {bad}",
                        result.stderr,
                    )

        mutable = (
            "::std::ptr::null_mut()",
            "(::std::ptr::null_mut())",
            "::core::ptr::null_mut()",
            "::std::ptr::addr_of_mut!(value)",
            "(::std::ptr::addr_of_mut!(value))",
        )
        for good in mutable:
            for position in (3, 4):
                with self.subTest(good=good, position=position):
                    args = ["a", "b", "c", "::std::ptr::null_mut()", "::std::ptr::null_mut()"]
                    args[position] = good
                    source = "fn fixture() {\n libc::openpty(\n " + ",\n ".join(args) + ",\n );\n}"
                    self.check_tree({"lib.rs": source}, 0)

    def test_redirected_descendants_keep_gates_and_loading_contexts(self):
        call = "fn f() { libc::openpty(a,b,c,d,&dimensions); }"
        gate = '#[cfg(target_os = "linux")] '
        for declaration, child in (("mod child;", "helpers/child.rs"),
                                   ("mod nested { mod child; }", "helpers/nested/child.rs")):
            with self.subTest(declaration=declaration):
                files = {"lib.rs": gate + '#[path = "helpers/provider.rs"] mod helper;',
                         "helpers/provider.rs": declaration, child: call}
                self.check_tree(files, 0)
                files["lib.rs"] = files["lib.rs"].replace(gate, "")
                self.check_tree(files, 1)

        files = {"lib.rs": gate + 'mod provider;\n#[path = "provider.rs"] mod redirected;',
                 "provider.rs": "mod child;", "provider/child.rs": call,
                 "child.rs": "pub fn portable() {}"}
        self.check_tree(files, 0)
        files["lib.rs"] = 'mod provider;\n' + gate + '#[path = "provider.rs"] mod redirected;'
        self.check_tree(files, 1)
        files["provider/child.rs"], files["child.rs"] = files["child.rs"], call
        self.check_tree(files, 0)
        files["lib.rs"] = files["lib.rs"].replace(gate, "")
        self.check_tree(files, 1)

    def test_redirected_child_is_still_an_automatic_cargo_target(self):
        files = {"../Cargo.toml": '[package]\nname = "fixture"\nversion = "0.1.0"\n',
                 "lib.rs": '#[cfg(target_os = "linux")] #[path = "bin/provider.rs"] mod helper;',
                 "bin/provider.rs": "mod child;",
                 "bin/child.rs": "fn main() { libc::openpty(a,b,c,d,&dimensions); }"}
        self.check_tree(files, 1)
        files["../Cargo.toml"] += "autobins = false\n"
        self.check_tree(files, 0)

    def test_macos_reachability(self):
        call = "fn f() { libc::openpty(a,b,c,d,&dimensions); }"
        self.check_tree({"lib.rs": '#[cfg(target_os = "linux")]\n' + call}, 0)
        self.check_tree({"lib.rs": '#[cfg(target_os = "linux")] mod child;', "child.rs": call}, 0)
        self.check_tree({"lib.rs": 'mod child;', "child.rs": call}, 1)
        self.check_tree({"lib.rs": '#[cfg(any(target_os = "linux", feature = "x"))]\n' + call}, 1)


if __name__ == "__main__":
    unittest.main()

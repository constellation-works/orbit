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

    def test_macos_reachability(self):
        call = "fn f() { libc::openpty(a,b,c,d,&dimensions); }"
        self.check_tree({"lib.rs": '#[cfg(target_os = "linux")]\n' + call}, 0)
        self.check_tree({"lib.rs": '#[cfg(target_os = "linux")] mod child;', "child.rs": call}, 0)
        self.check_tree({"lib.rs": 'mod child;', "child.rs": call}, 1)
        self.check_tree({"lib.rs": '#[cfg(any(target_os = "linux", feature = "x"))]\n' + call}, 1)


if __name__ == "__main__":
    unittest.main()

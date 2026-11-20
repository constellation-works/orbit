#!/usr/bin/env python3
"""Regression: a packed npm archive must contain exactly the runtime files."""
import importlib.util
from pathlib import Path
import tarfile
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("check_npm_package", SCRIPTS / "check_npm_package.py")
MODULE = importlib.util.module_from_spec(spec)
spec.loader.exec_module(MODULE)


class InspectArchiveTests(unittest.TestCase):
    def build(self, root, extra=False):
        npm = root / "npm"
        for name in MODULE.RUNTIME_FILES:
            path = npm / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("{}" if name == "package.json" else "runtime\n")
        names = list(MODULE.RUNTIME_FILES)
        if extra:
            extra_path = npm / "scripts/extra.js"
            extra_path.write_text("extra\n")
            names.append("scripts/extra.js")
        archive = root / "package.tgz"
        with tarfile.open(archive, "w:gz") as packed:
            for name in names:
                packed.add(npm / name, arcname=f"package/{name}")
        return archive

    def test_extra_packed_file_is_rejected(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as directory:
            root = Path(directory)
            archive = self.build(root, extra=True)
            with self.assertRaisesRegex(ValueError, "unexpected packed runtime files"):
                MODULE.inspect_archive(root, archive)

    def test_exact_runtime_files_are_accepted(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as directory:
            root = Path(directory)
            archive = self.build(root)
            files = MODULE.inspect_archive(root, archive)
            self.assertEqual(set(files), set(MODULE.RUNTIME_FILES))


if __name__ == "__main__":
    unittest.main()

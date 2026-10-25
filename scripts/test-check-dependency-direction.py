#!/usr/bin/env python3
"""Regress relative use-tree escapes through the public dependency guard."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent


class RelativeImportTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="dependency-direction-", dir=scratch)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        scripts = self.root / "scripts"
        scripts.mkdir()
        self.guard = scripts / "check-dependency-direction.sh"
        shutil.copy2(SCRIPTS / self.guard.name, self.guard)
        (self.root / "Cargo.toml").write_text("[workspace.dependencies]\n")
        manifest = self.root / "crates/orbit-core/Cargo.toml"
        manifest.parent.mkdir(parents=True)
        manifest.write_text('[package]\nname = "orbit-core"\n')
        metadata = self.root / "metadata.json"
        metadata.write_text(json.dumps({
            "workspace_members": ["orbit-core"],
            "packages": [{"id": "orbit-core", "name": "orbit-core",
                          "manifest_path": str(manifest), "dependencies": []}],
        }))
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        cargo = bin_dir / "cargo"
        cargo.write_text('#!/bin/sh\nexec cat "$GUARD_TEST_METADATA"\n')
        cargo.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}",
                        GUARD_TEST_METADATA=str(metadata))
        self.app = self.root / "crates/orbit-core/src/application"
        for directory in (
            "crates/orbit-core/src/application/contracts",
            "crates/orbit-core/src/application/task",
            "crates/orbit-core/src/application/job",
            "crates/orbit-core/src/application/automation",
            "crates/orbit-core/src/application/review",
            "crates/orbit-core/src/application/distributed",
            "crates/orbit-core/src/application/pull_drain",
            "crates/orbit-core/src/application/admission",
            "crates/orbit-core/src/application/workspace_auto",
            "crates/orbit-runtime/src", "crates/orbit-automation/src",
            "crates/orbit-store/src/contracts", "crates/orbit-store/src/driver/file",
            "crates/orbit-store/src/driver/sqlite", "crates/orbit-store/src/fs",
        ):
            (self.root / directory).mkdir(parents=True, exist_ok=True)

    def check_import(self, relative, source, rejected):
        path = self.app / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source + "\nfn probe() { let _: Option<Probe> = None; }\n")
        try:
            result = subprocess.run(["bash", str(self.guard)], env=self.env,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.stderr, "", result.stderr)
            self.assertEqual(result.returncode, 1 if rejected else 0, result.stdout)
        finally:
            path.unlink()

    def test_grouped_relative_imports_rejected(self):
        for module in ("distributed", "pull_drain"):
            for relative, tree in (
                ("job/worker.rs", f"super::super::{{{module}::Probe}}"),
                ("job/worker.rs", f"super::super::{{\n    RunOwnerLiveness,\n    {module}::{{Probe}},\n}}"),
                ("job/mod.rs", f"super::{{{module}::Probe}}"),
                ("job/pr_repair/adoption.rs", f"super::super::super::{{{module}::Probe}}"),
                ("job/worker.rs", f"super::{{super::{{{module}::Probe}}}}"),
            ):
                with self.subTest(module=module, relative=relative, tree=tree):
                    self.check_import(relative, f"use {tree};", rejected=True)

    def test_existing_direct_imports_rejected(self):
        for module in ("distributed", "pull_drain"):
            for prefix in ("crate::application", "super::super"):
                with self.subTest(module=module, prefix=prefix):
                    self.check_import("job/worker.rs", f"use {prefix}::{module}::Probe;",
                                      rejected=True)

    def test_within_job_imports_accepted(self):
        for relative, tree in (
            ("job/pr_repair/adoption.rs", "super::super::{RunOwnerLiveness, run_owner_liveness}"),
            ("job/worker.rs", "super::{distributed::Probe, pull_drain::Other}"),
            ("job/pr_repair/adoption.rs", "super::super::{distributed::Probe}"),
        ):
            with self.subTest(relative=relative, tree=tree):
                self.check_import(relative, f"use {tree};", rejected=False)

    def test_test_fixtures_accepted(self):
        for relative in ("job/tests/worker.rs", "job/tests.rs"):
            with self.subTest(relative=relative):
                self.check_import(relative,
                                  "use super::super::{distributed::Probe, pull_drain::Other};",
                                  rejected=False)


if __name__ == "__main__":
    unittest.main()

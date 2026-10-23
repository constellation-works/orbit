#!/usr/bin/env python3
"""Run desktop CI's audit command against a tiny offline advisory fixture.

Real Cargo resolves two independent workspaces and enforces their lockfiles.
A Cargo process adapter marks one local fixture dependency as registry-sourced
in metadata, so real cargo-deny applies RustSec advisories without downloading
any crate or index. Neither the advisory engine nor the workflow command is
stubbed. Missing cargo-deny/PyYAML is a failure, not a skipped regression.
"""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[1]
FIRST = "RUSTSEC-2026-9998"
UNKNOWN = "RUSTSEC-2026-9999"


class DesktopAdvisoryTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(shutil.which("cargo-deny"), "cargo-deny is required")
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="desktop-advisories-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "scripts").mkdir()
        shutil.copy2(ROOT / "scripts/cargo-deny.sh", self.root / "scripts/cargo-deny.sh")
        self.config = (ROOT / "deny.toml").read_text()
        (self.root / "deny.toml").write_text(self.config)
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("CARGO_DENY_", "ORBIT_CARGO_DENY_"))}
        self.env.update(CARGO_NET_OFFLINE="true", CARGO_TERM_COLOR="never",
                        CARGO_DENY_DISABLE_FETCH="1", TMPDIR=str(self.root),
                        CARGO_DENY_DB_PATH=str(self.root / "dbs"))
        self.write_crate("", "fixture-root")
        self.write_crate("desktop", "fixture-desktop", '''
[features]
default = ["native"]
native = ["dep:desktop-only"]
[dependencies]
desktop-only = { path = "../desktop-only", optional = true }
''')
        self.write_crate("desktop-only", "desktop-only")
        for manifest in ("Cargo.toml", "desktop/Cargo.toml"):
            self.run_ok(["cargo", "generate-lockfile", "--offline", "--manifest-path", manifest])
        self.db = self.root / "dbs/advisory-db-3157b0e258782691"
        (self.db / "crates/desktop-only").mkdir(parents=True)
        self.run_ok(["git", "init", "-q", str(self.db)])
        self.add_advisory(FIRST)
        # Cargo's metadata still selects the actual requested manifest/features
        # and checks --locked. Only this fixture crate's source is adapted.
        adapter = self.root / "fixture-cargo"
        adapter.write_text(f"#!{sys.executable}\n" + '''
import json, os, subprocess, sys
result = subprocess.run([os.environ["FIXTURE_REAL_CARGO"], *sys.argv[1:]],
                        stdout=subprocess.PIPE)
if result.returncode:
    sys.exit(result.returncode)
if "metadata" in sys.argv:
    metadata = json.loads(result.stdout)
    for package in metadata["packages"]:
        if package["name"] == "desktop-only":
            package["source"] = "registry+https://github.com/rust-lang/crates.io-index"
    print(json.dumps(metadata))
else:
    sys.stdout.buffer.write(result.stdout)
''')
        adapter.chmod(0o755)
        self.env.update(CARGO=str(adapter), FIXTURE_REAL_CARGO=shutil.which("cargo"))
        workflow = yaml.safe_load((ROOT / ".github/workflows/desktop.yml").read_text())
        self.audit = next(step["run"] for step in workflow["jobs"]["advisories"]["steps"]
                          if step.get("id") == "audit")

    def write_crate(self, folder, name, extra=""):
        directory = self.root / folder
        (directory / "src").mkdir(parents=True, exist_ok=True)
        (directory / "src/lib.rs").write_text("")
        (directory / "Cargo.toml").write_text(
            f'[workspace]\n[package]\nname = "{name}"\nversion = "1.0.0"\n'
            f'edition = "2021"\n{extra}')

    def run_command(self, command, env=None):
        return subprocess.run(command, cwd=self.root, env=env or self.env,
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                              text=True, timeout=60)

    def run_ok(self, command):
        result = self.run_command(command)
        self.assertEqual(result.returncode, 0, result.stdout)
        return result

    def add_advisory(self, advisory):
        (self.db / f"crates/desktop-only/{advisory}.md").write_text(f'''```toml
[advisory]
id = "{advisory}"
package = "desktop-only"
date = "2026-01-01"
url = "https://example.invalid/fixture"
informational = "unmaintained"
[versions]
patched = []
```
# Desktop-only maintenance fixture

Synthetic advisory for command-boundary coverage.
''')
        self.run_ok(["git", "-C", str(self.db), "add", "."])
        self.run_ok(["git", "-C", str(self.db), "-c", "user.name=Fixture",
                     "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false",
                     "commit", "-qm", advisory])

    def audit_desktop(self):
        # Execute the workflow's actual shell boundary, not a duplicated command.
        return self.run_command(["bash", "-euo", "pipefail", "-c", self.audit])

    def test_desktop_coverage_and_exact_id_exceptions(self):
        # The old root-only gate misses a dependency in the separate workspace.
        self.run_ok(["scripts/cargo-deny.sh", "--locked", "check", "advisories"])
        result = self.audit_desktop()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(FIRST, result.stdout, "the desktop advisory must cause the failure")
        (self.root / "deny.toml").write_text(self.config.replace(
            "ignore = [", f'ignore = [\n    {{ id = "{FIRST}", reason = "fixture only" }},', 1))
        result = self.audit_desktop()
        self.assertEqual(result.returncode, 0, result.stdout)
        self.add_advisory(UNKNOWN)
        result = self.audit_desktop()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(UNKNOWN, result.stdout, "an exact-ID exception cannot waive future advisories")

    def test_desktop_lockfile_cannot_be_rewritten(self):
        # First except the fixture so only the lock mismatch can fail the audit.
        (self.root / "deny.toml").write_text(self.config.replace(
            "ignore = [", f'ignore = [\n    {{ id = "{FIRST}", reason = "fixture only" }},', 1))
        manifest = self.root / "desktop-only/Cargo.toml"
        manifest.write_text(manifest.read_text().replace('version = "1.0.0"', 'version = "1.0.1"'))
        lock = self.root / "desktop/Cargo.lock"
        before = lock.read_bytes()
        result = self.audit_desktop()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertEqual(lock.read_bytes(), before, "the audit must preserve the committed lockfile")

    def test_missing_auditor_fails_closed(self):
        binary_dir = self.root / "bin"
        binary_dir.mkdir()
        for name in ("bash", "dirname"):
            (binary_dir / name).symlink_to(shutil.which(name))
        no_deny = binary_dir / "no-deny"
        no_deny.write_text("#!/bin/sh\nexit 1\n")
        no_deny.chmod(0o755)
        env = dict(self.env, PATH=str(binary_dir), CARGO=str(no_deny))
        result = self.run_command(["bash", "-euo", "pipefail", "-c", self.audit], env=env)
        self.assertNotEqual(result.returncode, 0, "missing cargo-deny must never skip the desktop gate")
        self.assertIn("cargo-deny not found", result.stdout)


if __name__ == "__main__":
    unittest.main()

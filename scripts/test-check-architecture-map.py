#!/usr/bin/env python3
"""Positive and negative fixture tests for scripts/check-architecture-map.py (STD-02 §R43)."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent
CHECKER = SCRIPTS / "check-architecture-map.py"


class ArchitectureMapGateTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="architecture-map-", dir=scratch)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

        (self.root / "Cargo.toml").write_text("[workspace]\nmembers = [\"crates/*\"]\n", encoding="utf-8")
        self.alpha_src = self.root / "crates/demo-alpha/src"
        self.beta_src = self.root / "crates/demo-beta/src"
        (self.alpha_src / "engine").mkdir(parents=True)
        (self.alpha_src / "tests").mkdir(parents=True)
        (self.beta_src / "cli").mkdir(parents=True)

        (self.root / "crates/demo-alpha/Cargo.toml").write_text(
            '[package]\nname = "demo-alpha"\nversion = "0.1.0"\n', encoding="utf-8"
        )
        (self.root / "crates/demo-beta/Cargo.toml").write_text(
            '[package]\nname = "demo-beta"\nversion = "0.1.0"\n', encoding="utf-8"
        )

        # Valid units with first-line //! docs; tests/ is excluded from top-level modules.
        (self.alpha_src / "lib.rs").write_text(
            "//! Core domain crate; imports no internal workspace crate.\n\n"
            "pub mod engine;\npub mod parser;\n",
            encoding="utf-8",
        )
        (self.alpha_src / "parser.rs").write_text(
            "//! Input parser; imports std only.\n", encoding="utf-8"
        )
        (self.alpha_src / "engine/mod.rs").write_text(
            "//! Execution engine; may import crate::parser.\n", encoding="utf-8"
        )
        (self.alpha_src / "tests/mod.rs").write_text(
            "// Tests are excluded from R43 top-level module mapping.\n", encoding="utf-8"
        )

        (self.beta_src / "main.rs").write_text(
            "//! Binary surface; may import demo-alpha.\n\nmod cli;\nfn main() {}\n",
            encoding="utf-8",
        )
        (self.beta_src / "cli/mod.rs").write_text(
            "//! Command declarations; may import demo-alpha.\n", encoding="utf-8"
        )

        arch_dir = self.root / "docs/architecture"
        arch_dir.mkdir(parents=True)
        (self.root / "ARCHITECTURE.md").write_text(
            "# Architecture\n\n"
            "## Crates\n\n"
            "| Crate | Tier | Owns | Depends on (internal) |\n"
            "|---|---|---|---|\n"
            "| [`demo-alpha`](docs/architecture/demo-alpha.md) | stable | Core domain logic. | — |\n"
            "| [`demo-beta`](docs/architecture/demo-beta.md) | internal | CLI surface. | demo-alpha |\n",
            encoding="utf-8",
        )
        (arch_dir / "demo-alpha.md").write_text(
            "# demo-alpha\n\n"
            "## What it owns\n\n"
            "Core domain crate; imports no internal workspace crate. Start at `crates/demo-alpha/src/lib.rs`.\n\n"
            "## Ownership by module\n\n"
            "- `engine`: Execution engine; may import `parser`.\n"
            "- `parser`: Input parser; imports `std` only.\n",
            encoding="utf-8",
        )
        (arch_dir / "demo-beta.md").write_text(
            "# demo-beta\n\n"
            "## What it owns\n\n"
            "Binary surface; may import `demo-alpha`. Entry point: `crates/demo-beta/src/main.rs`.\n\n"
            "## Ownership by module\n\n"
            "- `cli`: Command declarations; may import `demo-alpha`.\n",
            encoding="utf-8",
        )

        self.metadata_path = self.root / "metadata.json"
        self.write_metadata()

        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        cargo = bin_dir / "cargo"
        cargo.write_text('#!/bin/sh\nexec cat "$ARCH_MAP_TEST_METADATA"\n', encoding="utf-8")
        cargo.chmod(0o755)
        self.env = dict(
            os.environ,
            PATH=f"{bin_dir}:{os.environ['PATH']}",
            ARCH_MAP_TEST_METADATA=str(self.metadata_path),
        )

    def write_metadata(self, extra_packages=()):
        packages = [
            {
                "id": "demo-alpha",
                "name": "demo-alpha",
                "manifest_path": str(self.root / "crates/demo-alpha/Cargo.toml"),
                "targets": [
                    {
                        "name": "demo_alpha",
                        "kind": ["lib"],
                        "src_path": str(self.alpha_src / "lib.rs"),
                    }
                ],
            },
            {
                "id": "demo-beta",
                "name": "demo-beta",
                "manifest_path": str(self.root / "crates/demo-beta/Cargo.toml"),
                "targets": [
                    {
                        "name": "demo-beta",
                        "kind": ["bin"],
                        "src_path": str(self.beta_src / "main.rs"),
                    }
                ],
            },
            *extra_packages,
        ]
        self.metadata_path.write_text(
            json.dumps(
                {
                    "workspace_members": [pkg["id"] for pkg in packages],
                    "packages": packages,
                }
            ),
            encoding="utf-8",
        )

    def run_gate(self, env=None):
        return subprocess.run(
            [sys.executable, str(CHECKER), "--root", str(self.root)],
            env=env or self.env,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_valid_workspace_passes(self):
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("checked 2 crates and 3 top-level modules", result.stdout)

    def test_rejects_omitted_workspace_crate(self):
        arch = self.root / "ARCHITECTURE.md"
        arch.write_text(
            "\n".join(line for line in arch.read_text(encoding="utf-8").splitlines() if "demo-beta" not in line)
            + "\n",
            encoding="utf-8",
        )
        result = self.run_gate()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("missing workspace crate 'demo-beta'", result.stderr)

    def test_rejects_omitted_top_level_module(self):
        (self.alpha_src / "extra.rs").write_text("//! Extra unmapped module.\n", encoding="utf-8")
        result = self.run_gate()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("missing top-level module 'extra' for crate 'demo-alpha'", result.stderr)

    def test_rejects_mapped_module_that_does_not_exist(self):
        page = self.root / "docs/architecture/demo-alpha.md"
        page.write_text(
            page.read_text(encoding="utf-8") + "- `ghost`: Does not exist on disk.\n",
            encoding="utf-8",
        )
        result = self.run_gate()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("mapped module 'ghost' does not exist", result.stderr)

    def test_rejects_nonexistent_code_formatted_path_in_docs(self):
        for target_doc, snippet in (
            (self.root / "ARCHITECTURE.md", "\nSee `crates/demo-alpha/src/missing.rs`.\n"),
            (self.root / "docs/architecture/demo-alpha.md", "\nSee `crates/demo-alpha/src/ghost/`.\n"),
            (self.root / "docs/architecture/demo-alpha.md", "\nSee `engine/nonexistent.rs`.\n"),
        ):
            with self.subTest(doc=target_doc.name, snippet=snippet.strip()):
                original = target_doc.read_text(encoding="utf-8")
                target_doc.write_text(original + snippet, encoding="utf-8")
                try:
                    result = self.run_gate()
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    self.assertIn("documented path does not exist", result.stderr)
                finally:
                    target_doc.write_text(original, encoding="utf-8")

    def test_rejects_crate_root_without_first_line_doc_comment(self):
        (self.alpha_src / "lib.rs").write_text(
            "#![deny(clippy::print_stdout)]\n//! Doc comment not on line 1.\n",
            encoding="utf-8",
        )
        result = self.run_gate()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("crates/demo-alpha/src/lib.rs:1: missing first-line //! doc comment", result.stderr)

    def test_rejects_top_level_module_without_first_line_doc_comment(self):
        (self.alpha_src / "parser.rs").write_text("pub fn parse() {}\n", encoding="utf-8")
        result = self.run_gate()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("crates/demo-alpha/src/parser.rs:1: missing first-line //! doc comment", result.stderr)

    def test_rejects_nonexistent_mapped_crate_in_architecture_doc(self):
        arch = self.root / "ARCHITECTURE.md"
        arch.write_text(
            arch.read_text(encoding="utf-8")
            + "| [`demo-ghost`](docs/architecture/demo-ghost.md) | internal | Stale entry. | — |\n",
            encoding="utf-8",
        )
        result = self.run_gate()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("mapped crate 'demo-ghost' does not exist in cargo metadata", result.stderr)

    def test_fails_closed_when_cargo_metadata_fails(self):
        cargo = self.root / "bin/cargo"
        cargo.write_text("#!/bin/sh\necho 'metadata error' >&2\nexit 1\n", encoding="utf-8")
        cargo.chmod(0o755)
        result = self.run_gate()
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("cargo metadata failed", result.stderr)


if __name__ == "__main__":
    unittest.main()

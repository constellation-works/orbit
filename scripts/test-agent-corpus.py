#!/usr/bin/env python3
"""Regression for quoted frontmatter links containing a triple hyphen.

The generator previously split on any '---', truncating a valid source URL.
Exercise the public generator rather than its parser implementation.
"""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent


class CorpusFrontmatterRegression(unittest.TestCase):
    def test_hyphens_inside_a_link_do_not_end_frontmatter(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="corpus-frontmatter-", dir=scratch) as directory:
            repo = Path(directory)
            corpus = repo / "docs/agents/corpus"
            (corpus / "guidelines").mkdir(parents=True)
            (corpus / "guidelines/source-link.md").write_text(
                '---\ntype: guideline\napplies_to: [agent_implement]\nstatus: active\n'
                'links: ["https://example.test/a---b"]\n---\n'
                '# Source link\n\n## Rule\nUse source evidence.\n\n'
                '## Rationale\nLinks are data.\n\n## Escalation\nCheck unreadable sources.\n',
                encoding="utf-8",
            )
            for options in ([], ["--check"]):
                result = subprocess.run(
                    [sys.executable, str(ROOT / "scripts/agent-corpus.py"),
                     "--repo-root", str(repo), *options],
                    capture_output=True, text=True, check=False,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()

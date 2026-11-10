#!/usr/bin/env python3
"""Regression for quoted frontmatter links containing a triple hyphen.

The generator previously split on any '---', truncating a valid source URL.
Exercise the public generator rather than its parser implementation.
"""
from pathlib import Path
import json
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

    def test_materialize_appends_under_an_immediately_following_heading(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="corpus-adjacent-", dir=scratch) as directory:
            repo = Path(directory)
            corpus = repo / "docs/agents/corpus"
            target = corpus / "issues/adjacent-heading.md"
            target.parent.mkdir(parents=True)
            target.write_text(
                "---\n"
                "type: issue\n"
                "applies_to: [agent_implement]\n"
                "status: active\n"
                "links: []\n"
                "---\n"
                "\n"
                "# Adjacent heading\n"
                "\n"
                "## Signature\n"
                "Signature text.\n"
                "\n"
                "## Mechanism\n"
                "Mechanism text.\n"
                "\n"
                "## Resolution\n"
                "Resolution text.\n"
                "\n"
                "## Fix task\n"
                "Fix text.\n"
                "\n"
                "## Occurrences\n"
                "## After\n"
                "After text.\n",
                encoding="utf-8",
            )
            record = {
                "schema_version": 1,
                "id": "adjacent-heading",
                "path": "docs/agents/corpus/issues/adjacent-heading.md",
                "section": "Occurrences",
                "entry": {"summary": "adjacent-heading-entry"},
            }
            records = repo / "records.json"
            records.write_text(json.dumps({
                "history": [{
                    "event": "agent_corpus_append",
                    "by": "system",
                    "note": json.dumps(record),
                }],
            }), encoding="utf-8")
            result = subprocess.run(
                [sys.executable, str(ROOT / "scripts/agent-corpus.py"),
                 "--repo-root", str(repo), "--records", str(records)],
                capture_output=True, text=True, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            text = target.read_text(encoding="utf-8")
            heading = text.index("## Occurrences\n")
            line_end = text.index("\n", heading)
            next_heading = text.index("\n## ", line_end)
            marker = text.index("adjacent-heading-entry")
            self.assertGreater(marker, line_end)
            self.assertLess(marker, next_heading)


if __name__ == "__main__":
    unittest.main()

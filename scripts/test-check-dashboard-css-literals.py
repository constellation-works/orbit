#!/usr/bin/env python3
"""Regression tests for scripts/check-dashboard-css-literals.py."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent
CHECKER = SCRIPTS / "check-dashboard-css-literals.py"


class DashboardCssLiteralsTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        css_dir = self.root / "crates/orbit-web/assets/dashboard/css"
        css_dir.mkdir(parents=True)
        (css_dir / "base.css").write_text(":root { --font-size: 12px; }\n", encoding="utf-8")
        self.stylesheet = css_dir / "panel.css"
        scripts_dir = self.root / "scripts"
        scripts_dir.mkdir()
        (scripts_dir / "dashboard-css-literals-baseline.json").write_text(
            json.dumps({"schema_version": 1, "files": {}}) + "\n",
            encoding="utf-8",
        )

    def run_guard(self):
        return subprocess.run(
            [sys.executable, str(CHECKER), "--root", str(self.root)],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_accepts_url_fragments_strings_and_spaced_var_tokens_at_zero_baseline(self):
        self.stylesheet.write_text(
            '/* " */\n'
            '.x { filter: url(#abc); mask: url( #def ); content: "#face"; '
            "content: 'rgb(1, 2, 3)'; font-size: var( --font-size ); }\n"
            '.y { font-size: var(\n  --font-size\n) !important; }\n',
            encoding="utf-8",
        )
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("0 colour literals and 0 raw font sizes", result.stdout)

    def test_composite_base_token_and_equivalent_alpha_hints(self):
        (self.root / "crates/orbit-web/assets/dashboard/css/base.css").write_text(
            ":root { --outline: #fff thin; --accent-wash: rgba(122, 167, 255, 0.1); }\n",
            encoding="utf-8",
        )
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("0 colour literals and 0 raw font sizes", result.stdout)

        self.stylesheet.write_text(
            ".x { color: rgba(122,167,255,.104); background: rgb(122 167 255 / 10%); }\n",
            encoding="utf-8",
        )
        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(
            "rgba(122,167,255,.104); use a colour token from",
            result.stderr,
        )
        self.assertIn(
            "rgb(122 167 255 / 10%); use var(--accent-wash), the base.css token with this value",
            result.stderr,
        )

    def test_reports_nested_rgb_declaration_as_colour_rise(self):
        self.stylesheet.write_text(
            ".x { color: rgb(calc(10 + var(--n)) 20 30); }\n",
            encoding="utf-8",
        )
        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("1 colour literals, above its baseline of 0", result.stderr)
        self.assertIn("rgb(calc(10 + var(--n)) 20 30)", result.stderr)

    def test_string_containing_comment_token_does_not_hide_subsequent_literal(self):
        self.stylesheet.write_text(
            '.x { content: "/*"; color: #123; font-size: var(--font-size) 12px; }\n',
            encoding="utf-8",
        )
        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("1 colour literals, above its baseline of 0", result.stderr)
        self.assertIn("#123", result.stderr)
        self.assertIn("1 raw font sizes, above its baseline of 0", result.stderr)
        self.assertIn("var(--font-size) 12px", result.stderr)


if __name__ == "__main__":
    unittest.main()

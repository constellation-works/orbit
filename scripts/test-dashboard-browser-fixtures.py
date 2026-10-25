#!/usr/bin/env python3
"""Run the dashboard browser-fixture guard against small synthetic trees."""

from pathlib import Path
import self_test_hang
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent
CHECKER = SCRIPTS / "check-dashboard-browser-fixtures.py"

ASSETS = '<div id="keep-id" class="keep-class" data-keep="1"></div>\n'
ROUTES = 'fn router() { let _ = app.route("/tasks/:id", get(show)); }\n'
PASSING_FIXTURE = """\
import assert from 'node:assert/strict';
const panel = page.locator('#keep-id .keep-class[data-keep="1"]');
const task = document.getElementById('keep-id');
const kept = task.dataset.keep;
await fetch('/api/tasks/ABC-1');
assert.ok(panel && kept);
"""


class DashboardBrowserFixtureTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(
            prefix="dashboard-browser-fixtures-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def run_checker(self, fixture, assets=ASSETS):
        web = self.root / "crates/orbit-web"
        fixture_path = web / "src/tests/sample_browser.mjs"
        fixture_path.parent.mkdir(parents=True, exist_ok=True)
        fixture_path.write_text(fixture)
        assets_path = web / "assets/dashboard/index.html"
        assets_path.parent.mkdir(parents=True, exist_ok=True)
        assets_path.write_text(assets)
        routes_path = web / "src/api/routes.rs"
        routes_path.parent.mkdir(parents=True, exist_ok=True)
        routes_path.write_text(ROUTES)
        return self_test_hang.run(
            [sys.executable, str(CHECKER), "--root", str(self.root)],
            step="dashboard browser fixtures",
            capture_output=True, text=True,
        )

    def test_passing_tree_exits_zero(self):
        result = self.run_checker(PASSING_FIXTURE)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_removed_hooks_fail_naming_fixture_line_and_hook(self):
        cases = [
            ("id", "page.locator('#gone-id');\n", "gone-id"),
            ("class", "page.locator('.gone-class');\n", "gone-class"),
            ("data attribute", "page.locator('[data-gone=\"1\"]');\n", "data-gone"),
            ("data attribute", "const gone = node.dataset.gone;\n", "data-gone"),
            ("api path", "await fetch('/api/gone-route');\n", "/api/gone-route"),
        ]
        for kind, line, hook in cases:
            with self.subTest(kind=kind, hook=hook):
                # The queried hook sits on line 2 of the fixture.
                result = self.run_checker(f"const keep = 1;\n{line}")
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn(
                    f"crates/orbit-web/src/tests/sample_browser.mjs:2: "
                    f"missing {kind} hook '{hook}'",
                    result.stderr)

    def test_dynamic_hooks_exempt_only_what_the_template_builds(self):
        assets = (
            "<script>\n"
            "const id = `doctor-${check.name}`;\n"
            "node.dataset.key = `doctor-${index}-${row.check}`;\n"
            "node.id = `panel-${task.id}-crew`;\n"
            "el('div', { class: `chip chip-${tone}` });\n"
            "</script>\n")
        controls = [
            "page.locator('#panel-ABC-1-crew');\n",
            "page.locator('.chip-warn');\n",
        ]
        for line in controls:
            with self.subTest(control=line):
                result = self.run_checker(line, assets=assets)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        cases = [
            ("class", "page.locator('.doctor-command');\n", "doctor-command"),
            ("id", "page.locator('#chip-warn');\n", "chip-warn"),
            ("id", "page.locator('#panel-ABC-1');\n", "panel-ABC-1"),
        ]
        for kind, line, hook in cases:
            with self.subTest(kind=kind, hook=hook):
                result = self.run_checker(f"const keep = 1;\n{line}", assets=assets)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn(
                    f"crates/orbit-web/src/tests/sample_browser.mjs:2: "
                    f"missing {kind} hook '{hook}'",
                    result.stderr)


if __name__ == "__main__":
    unittest.main()

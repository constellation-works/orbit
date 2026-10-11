#!/usr/bin/env python3
"""Exercise the path filters that select the website validation workflow."""

from pathlib import Path
import re
import unittest

try:
    import yaml
except ImportError:
    yaml = None


ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github/workflows/website-check.yml"


def path_matches(pattern, path):
    """Match the `*` and `**` path patterns used by this workflow."""
    expression = []
    index = 0
    while index < len(pattern):
        if pattern[index] == "*":
            if pattern[index:index + 2] == "**":
                expression.append(".*")
                index += 2
            else:
                expression.append("[^/]*")
                index += 1
        else:
            expression.append(re.escape(pattern[index]))
            index += 1
    return re.fullmatch("".join(expression), path) is not None


@unittest.skipIf(yaml is None, "PyYAML not installed; the workflow guard skips locally without it")
class WebsiteWorkflowTests(unittest.TestCase):
    def setUp(self):
        document = yaml.safe_load(WORKFLOW.read_text(encoding="utf-8"))
        # PyYAML's YAML 1.1 loader treats the unquoted Actions key `on` as True.
        self.events = document.get("on", document.get(True, {}))

    def selects_website_checks(self, event, changed_paths):
        filters = self.events[event]
        if event == "push" and "main" not in filters.get("branches", []):
            return False
        patterns = filters.get("paths", [])
        return any(path_matches(pattern, path)
                   for pattern in patterns for path in changed_paths)

    def test_root_changelog_and_existing_inputs_select_checks_for_both_events(self):
        for event in ("pull_request", "push"):
            with self.subTest(event=event, path="CHANGELOG.md"):
                self.assertTrue(self.selects_website_checks(event, ["CHANGELOG.md"]))
            for path in ("website/src/pages/changelog.astro",
                         ".github/workflows/website-check.yml"):
                with self.subTest(event=event, path=path):
                    self.assertTrue(self.selects_website_checks(event, [path]))

    def test_unrelated_root_file_does_not_select_checks(self):
        for event in ("pull_request", "push"):
            with self.subTest(event=event):
                self.assertFalse(self.selects_website_checks(event, ["README.md"]))


if __name__ == "__main__":
    unittest.main()

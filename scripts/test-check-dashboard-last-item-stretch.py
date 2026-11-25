#!/usr/bin/env python3
"""Regression checks for the Task detail last-item stretch guard."""

from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent
CHECKER = SCRIPTS / "check-dashboard-last-item-stretch.py"


class DashboardLastItemStretchTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        css = self.root / "crates/orbit-web/assets/dashboard/css"
        css.mkdir(parents=True)
        self.task_detail = css / "task-detail.css"
        self.task_detail.write_text(
            ".comments-panel .comment-thread { display: flex; flex-direction: column; }\n"
            ".comments-panel .comment-thread > .comment-card { flex: none; }\n"
            ".row-detail .ac-list { display: flex; flex-direction: column; }\n"
            ".row-detail .ac-list > li { flex: none; }\n"
            ".row-detail .file-list { display: flex; flex-direction: column; }\n"
            ".row-detail .file-list > li { flex: none; }\n"
            ".task-run-list { display: flex; flex-direction: column; }\n"
            ".task-run { flex: none; }\n"
            ".readiness-list { display: flex; flex-direction: column; }\n"
            ".readiness-gap { flex: none; }\n"
            ".history-kv-list { display: flex; flex-direction: column; }\n"
            ".history-kv-list > * { flex: none; }\n"
            ".comment-outline { display: flex; flex-direction: column; }\n"
            ".comment-outline-link { flex: none; }\n"
            ".artifacts-list { display: grid; align-content: start; }\n"
            ".decision-answered-list { display: grid; align-content: start; }\n"
            ".decision-answers { display: grid; align-content: start; }\n"
            ".decision-fields { display: grid; align-content: start; }\n"
            ".decision-requests { display: grid; align-content: start; }\n"
            ".human-task-steps { display: grid; align-content: start; }\n"
            ".owed-criteria { display: grid; align-content: start; }\n"
            ".why-stuck-rows { display: grid; align-content: start; }\n"
            ".why-stuck-checks { display: grid; align-content: start; }\n"
            ".task-detail-pane .task-detail { flex: 1 0 auto; }\n"
            ".task-detail-body { flex: 1 1 auto; overflow-y: auto; }\n"
            ".field-editor-input { height: 100%; }\n"
            ".chart-frame { flex: 1; }\n",
            encoding="utf-8",
        )
        components = css / "components.css"
        components.write_text(".card { padding: 12px; }\n", encoding="utf-8")

    def run_guard(self):
        return subprocess.run(
            [sys.executable, str(CHECKER), "--root", str(self.root)],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_accepts_content_sized_stacks_and_parent_filling_regions(self):
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("keep content-sized items", result.stdout)

    def test_rejects_growth_on_each_task_detail_flex_stack(self):
        growth_rules = (
            ".comments-panel .comment-thread > .comment-card:last-child { flex: 1; }\n",
            ".row-detail .ac-list > li:last-child { flex-grow: 1; }\n",
            ".row-detail .file-list > li:last-child { flex: 1 1 auto; }\n",
            ".task-run-list > .task-run:last-child { flex-grow: 1; }\n",
            ".readiness-list > .readiness-gap:last-child { flex: auto; }\n",
            ".history-kv-list > *:last-child { flex: 1; }\n",
            ".history-kv-list > .history-kv-item { flex-grow: 1; }\n",
            ".history-kv-list .history-kv-item { flex: 1; }\n",
            ".comment-outline > .comment-outline-link:last-child { flex-grow: 1; }\n",
        )
        for rule in growth_rules:
            with self.subTest(rule=rule):
                original = self.task_detail.read_text(encoding="utf-8")
                self.task_detail.write_text(original + rule, encoding="utf-8")
                result = self.run_guard()
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("lets a stacked item", result.stderr)
                self.task_detail.write_text(original, encoding="utf-8")

    def test_accepts_item_descendant_growth_but_rejects_item_growth(self):
        descendant_rules = (
            ".comments-panel .comment-thread > .comment-card .comment-actions { flex: 1; }\n",
            ".comments-panel .comment-thread > .comment-card .comment-actions { flex-grow: 1; }\n",
        )
        for rule in descendant_rules:
            with self.subTest(rule=rule):
                original = self.task_detail.read_text(encoding="utf-8")
                self.task_detail.write_text(original + rule, encoding="utf-8")
                result = self.run_guard()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.task_detail.write_text(original, encoding="utf-8")

        original = self.task_detail.read_text(encoding="utf-8")
        self.task_detail.write_text(
            original + ".comments-panel .comment-thread > .comment-card:last-child { flex: 1; }\n",
            encoding="utf-8",
        )
        try:
            result = self.run_guard()
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("lets a stacked item", result.stderr)
        finally:
            self.task_detail.write_text(original, encoding="utf-8")

    def test_accepts_growth_inside_history_items(self):
        descendant_rules = (
            ".history-kv-list > .history-kv-item .history-kv-value { flex: 1; }\n",
            ".history-kv-list .history-kv-value { flex-grow: 1; }\n",
            ".history-kv-list strong:not(.history-kv-item) { flex: 1; }\n",
            ".history-kv-list strong:not(.history-kv-item) { flex-grow: 1; }\n",
        )
        for rule in descendant_rules:
            with self.subTest(rule=rule):
                original = self.task_detail.read_text(encoding="utf-8")
                self.task_detail.write_text(original + rule, encoding="utf-8")
                result = self.run_guard()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.task_detail.write_text(original, encoding="utf-8")

        original = self.task_detail.read_text(encoding="utf-8")
        self.task_detail.write_text(
            original + ".history-kv-list .history-kv-item { flex: 1; }\n",
            encoding="utf-8",
        )
        try:
            result = self.run_guard()
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("lets a stacked item", result.stderr)
        finally:
            self.task_detail.write_text(original, encoding="utf-8")

    def test_rejects_n_plus_k_child_on_star_stack(self):
        n_plus_k_rules = (
            ".history-kv-list > :nth-last-child(n+1) { flex: 1; }\n",
            ".history-kv-list > :nth-last-child(n + 1) { flex-grow: 1; }\n",
        )
        for rule in n_plus_k_rules:
            with self.subTest(rule=rule):
                original = self.task_detail.read_text(encoding="utf-8")
                self.task_detail.write_text(original + rule, encoding="utf-8")
                result = self.run_guard()
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("lets a stacked item", result.stderr)
                self.task_detail.write_text(original, encoding="utf-8")

    def test_rejects_growth_and_fixed_height_on_div_history_children(self):
        rules = (
            ".history-kv-list > div { flex: 1; }\n",
            ".history-kv-list > div:last-child { flex-grow: 1; }\n",
            ".history-kv-list > div:nth-last-child(1) { height: 200px; }\n",
            ".history-kv-list > .history-kv-item + div:last-child { flex: 1; }\n",
            ".history-kv-list > div.history-kv-item { flex: 1; }\n",
        )
        original = self.task_detail.read_text(encoding="utf-8")
        for rule in rules:
            with self.subTest(rule=rule):
                self.task_detail.write_text(original + rule, encoding="utf-8")
                try:
                    result = self.run_guard()
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    self.assertTrue(
                        "lets a stacked item" in result.stderr
                        or "gives a stacked item a fixed height" in result.stderr,
                        result.stdout + result.stderr,
                    )
                finally:
                    self.task_detail.write_text(original, encoding="utf-8")

    def test_rejects_zero_growth_scoped_to_unmatched_history_children(self):
        # Rendered history items are div.history-kv-item, so a zero-growth rule
        # scoped to other elements or classes never styles the items.
        baseline = ".history-kv-list > * { flex: none; }\n"
        original = self.task_detail.read_text(encoding="utf-8")
        self.assertIn(baseline, original)
        unmatched_rules = (
            ".history-kv-list > em { flex: none; }\n",
            ".history-kv-list > em:last-child { flex-grow: 0; }\n",
            ".history-kv-list > .history-kv-note { flex: none; }\n",
            ".history-kv-list > div.history-kv-note { flex: none; }\n",
        )
        for rule in unmatched_rules:
            with self.subTest(rule=rule):
                self.task_detail.write_text(original.replace(baseline, rule), encoding="utf-8")
                try:
                    result = self.run_guard()
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    self.assertIn("items need an explicit flex:none or flex-grow:0 rule", result.stderr)
                finally:
                    self.task_detail.write_text(original, encoding="utf-8")

        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_keeps_sibling_chain_for_stack_items(self):
        original = self.task_detail.read_text(encoding="utf-8")
        self.task_detail.write_text(
            original + ".comments-panel .comment-thread > .comment-card + .comment-card:last-child { flex: 1; }\n",
            encoding="utf-8",
        )
        try:
            result = self.run_guard()
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("lets a stacked item", result.stderr)
        finally:
            self.task_detail.write_text(original, encoding="utf-8")

        sibling_non_item = ".comments-panel .comment-thread > .comment-card + .comment-actions { flex: 1; }\n"
        self.task_detail.write_text(original + sibling_non_item, encoding="utf-8")
        try:
            result = self.run_guard()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        finally:
            self.task_detail.write_text(original, encoding="utf-8")

    def test_rejects_shared_card_growth_and_grid_row_stretch(self):
        components = self.root / "crates/orbit-web/assets/dashboard/css/components.css"
        components.write_text(".card { flex-grow: 1; }\n", encoding="utf-8")
        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("lets comment cards grow", result.stderr)

        components.write_text(".card { padding: 12px; }\n", encoding="utf-8")
        original = self.task_detail.read_text(encoding="utf-8")
        self.task_detail.write_text(
            original + ".decision-requests { align-content: stretch; grid-auto-rows: 1fr; }\n",
            encoding="utf-8",
        )
        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("lets grid rows stretch", result.stderr)
        self.assertIn("fills a stacked grid row", result.stderr)

    def test_rejects_flexible_rows_declared_apart_from_display_and_alignment(self):
        original = self.task_detail.read_text(encoding="utf-8")
        split_rows = (
            ".decision-requests { grid-auto-rows: 1fr; min-height: 20rem; }\n",
            ".decision-answers { grid-template-rows: 1fr; min-height: 20rem; }\n",
        )
        for rule in split_rows:
            with self.subTest(rule=rule):
                self.task_detail.write_text(original + rule, encoding="utf-8")
                result = self.run_guard()
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("fills a stacked grid row", result.stderr)

        self.task_detail.write_text(
            original + ".decision-requests { grid-auto-rows: auto; min-height: 20rem; }\n",
            encoding="utf-8",
        )
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()

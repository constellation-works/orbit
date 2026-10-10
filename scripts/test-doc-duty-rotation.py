#!/usr/bin/env python3
"""Behavior regression for the doc-duties rotation rule.

The rotation is an instruction algorithm in the bundled `doc-duties` template,
not compiled code, so this script executes a reference model of that algorithm
(carried per-path state, date precedence, repeat-skip hold, six-document batch)
against a simulated task history that, like `orbit.task.list`, exposes only the
newest 60 completed runs. It asserts outcomes (coverage, retained attempts,
holds), never template wording; update the model together with the template's
"Select a batch" and "Attach the ledger" sections.
"""

from __future__ import annotations

import sys
import unittest
from datetime import date, timedelta

WINDOW = 60
BATCH = 6
START = date(2026, 10, 1)


def day(n: int) -> str:
    return (START + timedelta(days=n)).isoformat()


class History:
    """Completed doc-duties tasks, oldest first; readers see a bounded window."""

    def __init__(self) -> None:
        self.ledgers: list[dict] = []

    def newest(self) -> list[dict]:
        return list(reversed(self.ledgers[-WINDOW:]))


def carried_state(history: History, candidates: set[str]) -> dict[str, list[dict]]:
    """Template step 2: newest v2 `state`, plus v1 rows read before it."""
    rows: dict[str, list[tuple[str, int, dict]]] = {}

    def add(path: str, row: dict, age: int) -> None:
        rows.setdefault(path, []).append((row["attempted_on"], -age, row))

    for age, ledger in enumerate(history.newest()):
        if ledger["schema_version"] == 2:
            for path, kept in ledger["state"].items():
                for row in kept:
                    add(path, row, age)
            break
        for doc in ledger["documents"]:
            add(doc["path"], doc, age)
    state = {}
    for path, found in rows.items():
        if path in candidates:
            found.sort(key=lambda item: (item[0], item[1]), reverse=True)
            state[path] = [row for _, _, row in found[:2]]
    return state


def select(candidates: dict[str, str], state: dict[str, list[dict]]):
    """Template steps 3-5. `candidates` maps path to its frontmatter/git date."""
    held, ordered = [], []
    for path, fallback in candidates.items():
        recent = state.get(path, [])
        if len(recent) == 2 and all(r["outcome"] in ("skipped", "partial") for r in recent):
            held.append(path)
            continue
        ordered.append((recent[0]["attempted_on"] if recent else fallback, path))
    ordered.sort()
    return [path for _, path in ordered[:BATCH]], held


def run_once(history, candidates, run_date, outcome_for=lambda path: "clean",
             version=2):
    candidates_set = set(candidates)
    state = carried_state(history, candidates_set)
    batch, held = select(candidates, state)
    documents = []
    for path in batch:
        row = {"attempted_on": run_date, "outcome": outcome_for(path)}
        if row["outcome"] in ("skipped", "partial"):
            row["reason"] = "too large"
        documents.append({"path": path, **row})
        state[path] = [row, *state.get(path, [])][:2]
    ledger = {"schema_version": version, "run_date": run_date, "documents": documents}
    if version == 2:
        ledger["state"] = state
    history.ledgers.append(ledger)
    return batch, held


def corpus(count: int) -> dict[str, str]:
    # Unchanged documents without frontmatter: git dates predate the first run
    # and increase with the path, so the oldest-first order is the path order.
    return {f"docs/d{i:03}.md": (START - timedelta(days=1000 - i)).isoformat()
            for i in range(count)}


class RotationTests(unittest.TestCase):
    def test_every_document_is_attempted_past_the_history_window(self):
        docs, history, seen = corpus(400), History(), set()
        for n in range(200):
            batch, _ = run_once(history, docs, day(n))
            self.assertLessEqual(len(batch), BATCH)
            seen.update(batch)
        self.assertEqual(len(seen), 400)

    def test_rotation_visits_all_before_repeating(self):
        docs, history, order = corpus(400), History(), []
        for n in range(67):  # ceil(400 / 6) runs; beyond the 60-run window
            order += run_once(history, docs, day(n))[0]
        self.assertEqual(len(set(order[:400])), 400)

    def test_window_only_retention_would_starve_late_documents(self):
        # Control: dropping `state` (the old v1 rule) reproduces the starvation,
        # so the scenarios above really cross the window boundary.
        docs, history, seen = corpus(400), History(), set()
        for n in range(200):
            seen.update(run_once(history, docs, day(n), version=1)[0])
        self.assertLess(len(seen), 400)

    def test_repeat_skip_hold_survives_window_eviction(self):
        docs, history = corpus(400), History()
        big = "docs/d000.md"

        def outcome(path):
            return "skipped" if path == big else "clean"

        n = 0
        while big not in run_once(history, docs, day(n), outcome)[1]:
            n += 1
            self.assertLess(n, 200, "never escalated after repeated skips")
        # The two skip attempts are now far outside the 60-run window.
        for m in range(n + 1, n + 150):
            batch, held = run_once(history, docs, day(m), outcome)
            self.assertNotIn(big, batch)
            self.assertIn(big, held)

    def test_latest_attempt_outranks_frontmatter_and_git(self):
        docs = {"docs/a.md": day(-500), "docs/b.md": day(-400)}
        state = {"docs/a.md": [{"attempted_on": day(5), "outcome": "clean"}]}
        batch, _ = select(docs, state)
        self.assertEqual(batch, ["docs/b.md", "docs/a.md"])

    def test_ties_break_by_path_and_batch_is_six(self):
        docs = {f"docs/{c}.md": day(-1) for c in "hgfedcba"}
        batch, _ = select(docs, {})
        self.assertEqual(batch, [f"docs/{c}.md" for c in "abcdef"])

    def test_v1_ledgers_in_window_carry_into_v2_state(self):
        docs, history = corpus(40), History()
        for n in range(3):
            run_once(history, docs, day(n), version=1)
        attempted = {d["path"] for ledger in history.ledgers for d in ledger["documents"]}
        batch, _ = run_once(history, docs, day(3))
        self.assertFalse(attempted & set(batch))
        state = history.ledgers[-1]["state"]
        self.assertTrue(attempted <= set(state))

    def test_state_drops_removed_documents(self):
        docs, history = corpus(12), History()
        run_once(history, docs, day(0))
        survivors = {p: d for p, d in docs.items() if p != "docs/d000.md"}
        run_once(history, survivors, day(1))
        self.assertNotIn("docs/d000.md", history.ledgers[-1]["state"])


if __name__ == "__main__":
    sys.exit(0 if unittest.main(exit=False).result.wasSuccessful() else 1)

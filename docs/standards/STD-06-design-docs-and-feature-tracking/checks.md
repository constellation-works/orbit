# Machine checks — STD-06 Design docs and feature tracking

The gate that enforces each rule in [STD-06](STD-06.md), or `review-only`. Ship a change
with the gate its rule names, or say it is review-only.

Each gate below is real and can be adopted. "Orbit:" says whether Orbit enforces it today.

| Rule | Gate |
|---|---|
| R1 | review-only. Optionally, a script lists folder slugs with no tracker tag of the same name, and tags with no folder. Orbit: none. |
| R2 | review-only. A generator can warn when a folder has no `1_overview.md` or `1_scope.md`. Orbit: `scripts/generate-doc-indexes.sh` falls back to the first top-level doc silently. |
| R3 | review-only. A CI `find docs/design -maxdepth 2 -iname 'README.md' -o -iname 'roadmap.md' -o -iname 'changelog.md'` that fails on any hit (outside `_archived/` and `_templates/`). Orbit: none. |
| R4 | review-only. A frontmatter lint over `docs/design/*/[0-9]_*.md`. Orbit: none (CONVENTIONS §11 lists it as a future lint). |
| R5 | The board generator fails on a missing or unknown `stage`. Orbit: `scripts/generate-doc-indexes.sh`, run by `make ci-guards` with `--check`. |
| R6 | review-only. The generator renders a missing `next` as `—`, so an absent line shows on the board. |
| R7 | review-only. A reviewer of a change that approves, lands or parks feature work checks the entry doc's `stage` and `next`. |
| R8 | review-only. |
| R9 | `<generator> --check` in CI (STD-04 §R11). Orbit: `scripts/generate-doc-indexes.sh --check` in `ci-guardrails.sh`. |
| R10 | review-only. Optional lints: every entry has a `Cost:` line; every `Code anchors`/`Paths` path exists. Orbit: none (CONVENTIONS §11 checks 2 to 4). |
| R11 | review-only. A link checker resolves title anchors. Orbit: none for decision anchors. |
| R12 | review-only. The filing checklist of whoever files tasks (in the constellation, the `orchestration-session` skill §4) requires the slug. A tracker query for open tasks whose footprint touches a feature's `paths` but lack its tag can flag misses. Orbit: none. |
| R13 | review-only. |
| R14 | review-only. The generator lists `_archived/` separately. Orbit: yes. |

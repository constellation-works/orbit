# Gate rules

The full text behind the `## Gates` and `## Local Rust CodeQL` bullets of the root [`AGENTS.md`](../../AGENTS.md): which checks a diff runs before handoff, and when local CodeQL is owed.

## Gates

- There is no required local full-suite gate for a diff. The required PR checks (Check / Clippy / Test and Test shards 1–3) cover formatting, lint, goldens and the full suite; a red check is repaired through the PR. Don't run `make ci`, `ci-fast`, `ci-lint`, `goldens` or `ci-test-affected` as blanket pre-handoff gates. Run what the change needs: tests of the changed behavior and `make goldens` when CLI help, MCP text or another golden surface changes (`UPDATE=1` regenerates; review the diff). A managed delivery's final validation runs `task.final_validation_commands`. See [DEVELOPMENT.md](../DEVELOPMENT.md#validation-and-ci).
- Before handing off a diff, run `make ci-guards` and `cargo fmt --all -- --check`. Also run `rustfmt --check --edition <crate edition> <path>…` for changed test `.rs` files. For each changed crate, run the production, all-target and rustdoc checks in [Validation and CI](../DEVELOPMENT.md#validation-and-ci). Use the shared lint wrapper for the root workspace and strict Cargo Clippy for desktop.
- When an edit changes a generated row of `docs/INDEX.md` or the feature board in `docs/design/FEATURES.md` (indexed frontmatter, first heading or summary), run `make docs-index` and `scripts/generate-doc-indexes.sh --check`, and commit the generated changes with the source.
- `no-diff-expected` tasks run no workspace gates or validation commands and attach no `no-diff.json` or validation logs; run only what the brief requires.
- `skip-validation` and `skip-review` skip local validation (including docs-index) and review only while every changed path matches `workflow.light_delivery_paths`; any other path voids both. Only filing or an operator sets them. See [CONFIG.md](../CONFIG.md#light-delivery-skip-review-and-skip-validation).

## Local Rust CodeQL

Hosted CodeQL (`.github/workflows/codeql.yml`) analyses `main` pushes and runs daily on `agent-main`, not on PRs. A docs-only push whose parent already has a successful completed run of this workflow skips the analysis jobs; the parent's code-scanning result still covers that unchanged code. A non-docs path, a parent without that proof, and the daily schedule still run every analysis job, as does `workflow_dispatch`. Its alerts become repair tasks. Run it locally only when the task repairs a CodeQL finding, is tagged `security` or `security-review`, or edits `.github/codeql/**` or `scripts/codeql-rust-local.sh`. Then follow the [local CodeQL runbook](../runbooks/codeql-local.md):

- Extraction checks are required evidence; results from incomplete extraction can't confirm a repair.
- Run once per delivery against the final candidate, in the foreground with a timeout of at least 45 minutes, and read that run's `results.sarif`. An interrupted run starts over.
- Only Linux extracts every module. Elsewhere the script exits 3, and the check is owed by a Linux run recorded as `codeql` external evidence.

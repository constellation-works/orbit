# Exemplars — STD-06 Design docs and feature tracking

Worked examples of the rules in [STD-06](STD-06.md), as pointers into the Orbit codebase. Orbit is the example, not the audience: this file is optional reading, and useful only with an Orbit checkout.

Paths are relative to `codebases/orbit/`.

- R1–R3 — `docs/design/CONVENTIONS.md` §1 (the feature folder) and §10 (what not to create); `docs/design/_templates/full/`. At v5 Orbit's CONVENTIONS and its `_templates/scope/` still describe the retired scope track and retire with it when Orbit syncs.
- R2 — `docs/design/path-leases/` (overview, design, vision, decisions, references). Orbit's `1_scope.md` docs, such as `docs/design/usage-pressure/1_scope.md` and `docs/design/crew-credentials/1_scope.md`, are the migration backlog.
- R4–R6 — `docs/design/CONVENTIONS.md` §2; `docs/design/experiments/1_scope.md` frontmatter (`stage: building` with a `next` naming the in-flight slice).
- R9 — `scripts/generate-doc-indexes.sh#render_feature_board` and the "Feature board" section of `docs/INDEX.md`.
- R10–R11 — `docs/design/CONVENTIONS.md` §4a to §4e; `docs/design/_templates/full/4_decisions.md`.
- R12 — Orbit tasks tagged `auto-concurrency`, `operations-as-data` and `path-leases`.
- R14 — `docs/design/CONVENTIONS.md` §7 "Archiving a design"; `docs/design/_archived/README.md`.

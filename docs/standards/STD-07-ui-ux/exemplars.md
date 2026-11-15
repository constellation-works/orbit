# Exemplars — STD-07 UI and UX

Worked examples of the rules in [STD-07](STD-07.md), as pointers into the Orbit
codebase. Orbit is the example, not the audience: this file is optional reading, and
useful only with an Orbit checkout.

- R1, R8, R11 — `docs/design/user-interface/specs/theme.md#Design Tokens` and its "Text
  conventions": the shared spacing, type and colour tokens, so a view sets layout but never
  its own font or colour.
- R8 — `crates/orbit-web/assets/dashboard/css/components.css`: the shared `th` / `.col-head`
  and `.section-title` rules.
- R1–R4, R6, R9, R10, R13 — `scripts/ux-rules/ui/`: one module per machine rule, each with
  its targets, widths and states.
- Rule-run manifest — `crates/orbit-web/visual/views.toml`: the views, widths, themes and
  states the UI rules expand over.
- R26 — `docs/design/ux-rules/1_scope.md`: how a recurring visual defect becomes a rule.

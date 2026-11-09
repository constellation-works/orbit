# Machine checks — STD-07 UI and UX

The gate that enforces each rule in [STD-07](STD-07.md), or `review-only`. Ship a change
with the gate its rule names, or say it is review-only.

The automated gates are Orbit's UI rule set, `scripts/ux-rules/ui/*.mjs`. It runs by hand
with `node scripts/ux-rules/run.mjs --surface ui` against a live URL or a fixture, and in
the QA full sweep's `ux-measurement-browser` scenario. A project without that runner does
the same review by hand at 375px, 768px and 1440px in each theme it ships.

| Rule | Gate |
|---|---|
| R1 | `ui.sibling-spacing`, `ui.cramped-padding`, `ui.panel-body-padding` |
| R2 | `ui.even-row-panels`, `ui.aligned-starts` |
| R3 | `ui.aligned-starts` |
| R4 | `ui.text-overflow`, `ui.text-occlusion`, `ui.no-horizontal-overflow` (375px included) |
| R5 | review-only: open, load and dismiss each transient element, and compare neighbour positions |
| R6 | `ui.control-size` |
| R7 | review-only |
| R8 | `ui.undersized-text`, `ui.heading-rhythm`, `ui.tight-leading`; sibling font size and weight are checked in review |
| R9 | `ui.cramped-wrap` |
| R10 | `ui.contrast` (text); section and idle-element separation is checked in review |
| R11 | review-only: no colour literal outside the token file |
| R12 | review-only |
| R13 | `ui.bounded-length` |
| R14 | `ui.bounded-length`; the untruncated-error exception is checked in review |
| R15 | review-only |
| R16 | review-only |
| R17 | review-only |
| R18 | review-only |
| R19 | review-only: check each chart against the table or ranking it summarises |
| R29 | review-only: no panel whose body is one figure and a caption |
| R20 | review-only |
| R21 | `ui.console-clean` (no error noise); expiry and dismissal are checked in review |
| R22 | review-only |
| R23 | `ui.no-horizontal-overflow` at the manifest widths; second-column use is checked in review |
| R24 | review-only |
| R25 | review-only: the task links the approved mock |
| R27 | review-only at 375px, 768px and 1440px; a candidate for a new `ui.orphan-row` rule (count the items on each row of a wrapping flex or grid container, and fail when the last row holds one item and the rows above hold more) |
| R28 | `ui.aligned-starts` covers left edges; right edges are review-only at 1440px. A candidate for extending `ui.even-row-panels` to compare the right edges of stacked siblings |
| R30 | review-only: one CSS class or component per kind of element, never restyled per view; `ui.cramped-wrap` catches wrapped step labels |
| R31 | `ui.nested-card` |
| R26 | review-only: the fix task quotes the correction and names the rule, or the friction it filed |

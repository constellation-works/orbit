---
type: design
summary: "Spec: Canon Refined Theme"
tags: ["user-interface"]
last_validated: 2026-11-21
---

# Spec: Canon Refined Theme

This document defines the formal design tokens and visual language for the Orbit User Interface (Canon Refined aesthetic), superseding the deprecated Trading Terminal theme.

## Why This Exists

As Orbit matures, the extreme constraints of the "Trading Terminal" aesthetic (pitch black, pure monospace, sharp 0px corners) proved too rigid for complex, hierarchical data presentation like nested task plans, conversational review threads, and rich telemetry. The "Canon Refined" theme provides a balanced, modern, high-density dashboard language that maintains a "pro-tool" feel while adopting established UI affordances (subtle rounding, sans-serif readability, softer semantic colors).

## Design Tokens

### Background & Elevation
The theme uses a layered dark mode, relying on subtle lightness shifts rather than shadows. The values are cool blue-greys: the canvas, the chrome and the cards differ by small steps of lightness.
- `--bg`: `#0b0d10` (Base canvas)
- `--bg-chrome`: `#0e1115` (The top bar and the status bar)
- `--bg-rail`: `#0e1115` (Navigation rail; the same surface as the chrome)
- `--bg-elev`: `#121519` (Cards and panels)
- `--bg-control`: `#151920` (A raised control on a card: selects, steppers, badges)
- `--bg-well`: `#0f1216` (A well one step below a card: stat tiles, segmented-control tracks, expanded detail rows)
- `--bg-sunk`: `#0f1216` (Sticky group headings)
- `--bg-selected`: `#1a1f27` (The selected rail entry, chip or segment)

### Borders
Borders delineate structure without heavy contrast.
- `--border`: `#22272e` (Panel and control edges)
- `--hair`: `#1b1f25` (Dividers inside panels and controls)
- `--border-strong`: `#2e343c` (A selected segment or chip, and the edge of a secondary button)
- Focused inputs use the `--accent` border.

### Typography
- **Sans-serif (Primary):** `Geist`, self-hosted, used for prose, titles, and general UI text.
- **Monospace (Secondary/Data):** `Geist Mono`, self-hosted, used for IDs, metrics, timestamps, code snippets and logs.
- **Base Size:** `14px` (`--text-base`) with `1.5` line height.

### Type-Size Scale
All `font-size` declarations in the dashboard use the ten integer steps in `base.css`. Half-pixel sizes (`9.5px`, `10.5px`, `11.5px`, `12.5px`, `13.5px`, `14.5px`) are excluded from the scale; snap them to the adjacent step for their role so sub-pixel drift does not accumulate.

| Token | Value | Common uses (and off-scale snaps) |
|---|---|---|
| `--text-2xs` | `10px` | Micro chart axis ticks and compact inline tags that are not content prose (snaps `9px`, `9.5px`, micro `10.5px`). Content-bearing UI text stays at `11px` or above. |
| `--text-xs` | `11px` | Label text: `th`, `.col-head`, uppercase field overlines, compact badges and alert counts (snaps `10.5px` content labels and `11.5px` badges). |
| `--text-sm` | `12px` | Secondary and data text: table cells, metadata, descriptions, monospace IDs/metrics/timestamps, rail volume counts, compact buttons/chips/pills/inputs (snaps `11.5px` log/data lines and `12.5px` controls/metadata). |
| `--text-md` | `13px` | Section titles (`.section-title`), primary list/row titles, nav and dock tabs, panel body prose and empty states (snaps `12.5px` readable prose and `13.5px` tabs/row titles). |
| `--text-base` | `14px` | Body baseline (`html, body`), `.panel > header` titles, primary prose and markdown headings (snaps `13.5px` body prose and `14.5px`). |
| `--text-lg` | `16px` | Detail and dialog subheadings (`h2`/`h3`), medium card figures (covers `15px`–`17px`). |
| `--text-xl` | `18px` | View and pane titles (`.topbar .crumb`, `.task-pane-title`), medium stat-tile figures. |
| `--text-2xl` | `20px` | Stat-tile KPI numbers and file-view headings (covers `20px`–`22px`). |
| `--text-3xl` | `24px` | Large callout figures and empty-state icons. |
| `--text-4xl` | `28px` | Hero KPI numbers (covers `28px`–`30px`). |

Relative `em` sizing stays allowed only where scaling a glyph proportionally with its host text (for example `.copy-id::after` at `0.85em`).

### Spacing Scale
All `padding`, `margin` and `gap` declarations use the twelve steps in `base.css`. Intermediate literals (`3px`, `5px`, `7px`, `9px`, `11px`, `18px`, `22px`, `28px`) snap to the nearest step without changing visible rhythm, and container padding stays symmetric (`top == bottom`, `left == right` per STD-07 R1). (`1px` grid gaps over a `--border` background are structural hairlines, not spacing steps.)

| Token | Value | Role |
|---|---|---|
| `--space-2` | `2px` | Hairline offsets, tight stacked row lines, minimal tag vertical padding |
| `--space-4` | `4px` | Micro gaps (status dot to label, icon to text), compact badge/tag vertical padding |
| `--space-6` | `6px` | Tight gaps inside controls (chip, pill, button, inline action groups), table cell vertical padding |
| `--space-8` | `8px` | Standard gap between sibling controls in toolbars and button rows; compact row vertical padding; scrollbars and tight insets |
| `--space-10` | `10px` | Standard row vertical padding, control horizontal padding, dense card/box padding |
| `--space-12` | `12px` | Standard horizontal row/control padding, section/column stack gaps, compact panel/card inset, narrow-screen row padding |
| `--space-14` | `14px` | Panel header vertical padding (`14px 16px`), medium card inset |
| `--space-16` | `16px` | Standard panel inset, full-width row horizontal padding, major toolbar/panel padding, section stack gaps |
| `--space-20` | `20px` | Generous section/detail header horizontal inset, list indents, status-bar horizontal padding |
| `--space-24` | `24px` | Page-level grid padding and gaps, empty-state horizontal padding, wide meta-grid column gaps |
| `--space-32` | `32px` | Wide split-layout column gaps and generous empty-panel vertical padding |
| `--space-48` | `48px` | Full empty-state vertical padding (`--space-48` `--space-24`) |

#### Common spacing choices
- **Row padding:** `--space-8` `--space-12` (`8px 12px`) or `--space-10` `--space-12` (`10px 12px`) for dense table/list rows; `--space-10` `--space-16` (`10px 16px`) or `--space-12` `--space-16` (`12px 16px`) for full-width panel rows (settings, expanded detail); `--space-8` `--space-16` or `--space-4` `--space-16` for fixed-min-height list rows (`tasks`, `runs`); `--space-12` (`12px`) on narrow stacked rows.
- **Panel inset:** `--space-14` `--space-16` (`14px 16px`) or `--space-12` `--space-16` (`12px 16px`) for panel headers and toolbars (`.controls`); `--space-16` (`16px`) or `--space-12` `--space-16` for panel bodies and cards; `--space-12` (`12px`) or `--space-10` `--space-12` for nested cards/wells; `--space-8` `0` for bare list panel bodies whose rows carry their own horizontal inset; `--space-48` `--space-24` for `.empty-state`.
- **Gaps between controls:** `--space-8` (`8px`) between sibling buttons, filters, chips and inputs in a toolbar or action row; `--space-6` (`6px`) inside a control (icon + label) or between tightly paired inline actions; `--space-4` (`4px`) for dot-to-label or compact tag clusters; `--space-12` (`12px`) or `--space-16` (`16px`) between adjacent cards or form groups.

### Radius Scale
All `border-radius` declarations use the six steps in `base.css` (with `50%` reserved for 1:1 circles such as status dots and avatars).

| Token | Value | Common uses (and off-scale snaps) |
|---|---|---|
| `--radius-2xs` | `2px` | Inline progress bars, chart bars, colour swatches, focus-ring offsets on plain text controls (covers `1px`–`3px`) |
| `--radius-xs` | `4px` | Badges, tags, skeleton rows, compact inline markers (covers `3px`–`5px`) |
| `--radius-sm` | `6px` | Buttons, text inputs, segments, small nested wells (covers `5px`–`7px`, snapping `7px` controls) |
| `--radius` | `8px` | Stat tiles, segmented-control tracks, selects, inner cards/boxes, top corners of attached tabs (`var(--radius) var(--radius) 0 0`) |
| `--radius-panel` | `10px` | Panels, top-level cards, dialogs and drawers (covers `10px`–`14px`) |
| `--radius-pill` | `999px` | Filter chips, status pills and toggle switches (covers `999px` and `13px`–`16px` capsules) |

### Text conventions
The shared classes live in `crates/orbit-web/assets/dashboard/css/components.css`; a view sets layout on them (padding, alignment, stickiness) but never font, case or tracking.
- **Column heads:** every `<th>` takes one style from the shared `th` rule, and every grid header row that stands in for a table head (Tasks, Runs, Automation, Settings crews, system and hosts) carries the same rule as `.col-head`: sans, `11px`, weight `600`, uppercase, `0.06em` tracking, `--fg-dim`. Numeric columns align right but keep the sans head; mono is for the cells.
- **Titles:** sentence case: capitalize only the first word and proper names (`MCP vs CLI`, `Knowledge pack`, `Context files`). A panel header is `14px` sans, weight `600`. A section or card title inside a panel uses `.section-title`: `13px` sans, weight `600`, `--fg`. Titles are never `text-transform: capitalize`, which raises every word of generated text (`Average Implement_one Duration By Actor`). A title built from lowercase data raises only its first letter through `.section-title::first-letter`.
- **Overlines:** the small uppercase style is for column heads and field labels inside a section, never for a title.
- **Prose:** explanatory notes, hints, empty states, placeholders and error or status messages are sans. Mono stays for IDs, metrics, timestamps, code, and raw diagnostics or logs, even inside a sentence (`code` spans).
- **Rail counts:** a rail count is either an attention count or a volume count. An attention count (failed runs) is the `.rail-count.alert` pill in the danger colours. A volume count (tasks listed, audited events) is plain `12px` `--fg-mute` numerals that do not brighten on the active entry. Colour or a pill on a volume count would give a total the weight of something that needs action.

### Semantic Colors
Colors are muted but distinct, avoiding harsh neon tones while maintaining semantic meaning.
- **Text:** `--fg-strong` (`#f2f4f7`, titles and selected entries), `--fg` (`#e7eaee`), `--fg-soft` (`#c9d0d9`, rail entries, field labels and long-form body text such as task descriptions, failure messages and friction reports), `--fg-dim` (`#a3acb8`), `--fg-mute` (`#8b95a3`)
- **Disabled text:** `--fg-disabled` (`#66707d`), reserved for disabled states. Muted text stays above 4.5:1 contrast on `--bg`, `--bg-rail` and `--bg-elev`; disabled plugin cards keep full text contrast so their status and diagnostics remain readable.
- **Accent (Blue):** `--accent` (`#7aa7ff`)
- **Success/Done (Green):** `--status-done` (`#63d297`)
- **In-Progress (Cyan):** `--status-in-progress` (`#5cc8f0`)
- **Review (Purple):** `--status-review` (`#b794f6`)
- **Warning/Proposed (Amber):** `--status-proposed` (`#f2b24c`)
- **Backlog (Grey):** `--status-backlog` (`#8a94a3`)
- **Error/Blocked (Red):** `--status-blocked` (`#ff7a6b`)
- **Warning note:** `--warn-wash` (`#1c160b`), `--warn-line` (`#43331a`) and `--warn-fg` (`#f2c46d`) draw an amber note or chip: a host or provider hold, a blocking readiness gap, Winding down, a WRN log badge.
- **Danger note:** `--danger-wash` (`#150f0f`), `--danger-line` (`#3a2321`) and `--danger-fg` (`#ff9c90`) draw the blocked group, a critical host reading, Stop and an ERR log badge.

Status colours are lighter than the Tailwind 500 steps they replaced so a 7px dot stays distinct on the near-black canvas; each is paired with a word, so colour is never the only signal.

### Readiness colours
A backlog task's readiness dot, the Backlog card's readiness bar and its legend share one colour per drain pool. Each differs from its neighbours in lightness as well as hue, and each dot also carries its pool in words (accessible name and title).
- `--ready-eligible`: `#63d297` (Eligible now)
- `--ready-locks`: `#d9902f` (Waiting on locks)
- `--ready-capacity`: `#4f7fe0` (Waiting on capacity)
- `--ready-other`: `#66707d` (Waiting, other)

`[data-pool="…"]` sets `--dot` from these, so a dot, a bar segment and a legend swatch pick up their colour from the attribute alone.

### Structural Rules
- **Keyboard focus:** Every focusable control uses a `2px solid var(--accent)` outline under `:focus-visible`, including filters and task-action textareas. Outline offsets may move the ring inside a clipped row or outside a control.
- **Radii:** Defined by the [Radius Scale](#radius-scale): `--radius-panel` (`10px`) for panels, cards and dialogs; `--radius` (`8px`) for stat tiles, segmented-control tracks and selects; `--radius-sm` (`6px`) for buttons, inputs and segments; `--radius-xs` (`4px`) for badges and tags; `--radius-2xs` (`2px`) for bars and swatches; and `--radius-pill` (`999px`) for filter chips and pills (`50%` for 1:1 status dots).
- **Density:** Tight and uniform via the [Spacing Scale](#spacing-scale) (e.g., `--space-14` `--space-16` or `--space-12` `--space-16` for panel headers, `--space-8` gaps between controls), while text breathes at `--text-base` / `1.5` line height.
- **Animation:** Minimal, purposeful motion. Used primarily for loading indicators (e.g., `pulse-skeleton 1.5s infinite ease-in-out`).

### Label/value lists
Detail panes and side panels list facts as label/value pairs (for example a "Shown, not scored" list in a crew detail pane). A reviewer can check each of these from a screenshot:
- **No collisions:** A label and its value never overlap or touch. A long value wraps inside its own column, and a long composite value (a count followed by a qualifier) wraps rather than running under the label.
- **Whole labels:** A label never breaks into one word per line. When the label and value cannot sit side by side, the value moves under its label instead of squeezing the label column.
- **Label column width:** The label column keeps a readable minimum width of about `12ch`, and a label that needs more room takes it from the value column.
- **Numbers:** Numeric values stay right-aligned when they sit beside their label, so digits line up down the list.

## Mechanism-specific sections

### Expandable Rows
Data tables use expandable rows (`.row.expanded`). When expanded:
- The row background shifts to a faint accent tint (`rgba(110, 159, 255, 0.05)`) in the audit and run-step tables.
- The expanded detail view uses `--bg-well` with a 2-column layout (main content + side metadata).
- Tasks are the exception: the selected task row takes `--bg-selected`, and its detail opens in a side pane on `--bg-elev` with tabs instead of under the row. The pane's Overview gains its side metadata column only when the pane is wide enough.
- Collapsible field carets turn 90 degrees, from `rotate(45deg)` open to `rotate(-45deg)` collapsed, for clear state indication.

## Agent Signature
gemini

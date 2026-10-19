---
type: design
summary: "Spec: Canon Refined Theme"
tags: ["user-interface"]
last_validated: 2026-10-17
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
- **Base Size:** `14px` with `1.5` line height.

### Text conventions
The shared classes live in `css/components.css`; a view sets layout on them (padding, alignment, stickiness) but never font, case or tracking.
- **Column heads:** every `<th>` takes one style from the shared `th` rule, and every grid header row that stands in for a table head (Tasks, Runs, Automation, Settings crews, system and hosts) carries the same rule as `.col-head`: sans, `11px`, weight `600`, uppercase, `0.06em` tracking, `--fg-dim`. Numeric columns align right but keep the sans head; mono is for the cells.
- **Titles:** sentence case: capitalize only the first word and proper names (`MCP vs CLI`, `Knowledge pack`, `Context files`). A panel header is `14px` sans, weight `600`. A section or card title inside a panel uses `.section-title`: `13px` sans, weight `600`, `--fg`. Titles are never `text-transform: capitalize`, which raises every word of generated text (`Average Implement_one Duration By Actor`). A title built from lowercase data raises only its first letter through `.section-title::first-letter`.
- **Overlines:** the small uppercase style is for column heads and field labels inside a section, never for a title.
- **Prose:** explanatory notes, hints, empty states, placeholders and error or status messages are sans. Mono stays for IDs, metrics, timestamps, code, and raw diagnostics or logs, even inside a sentence (`code` spans).
- **Rail counts:** a rail count is either an attention count or a volume count. An attention count (failed runs) is the `.rail-count.alert` pill in the danger colours. A volume count (tasks listed, audited events) is plain `12px` `--fg-mute` numerals that do not brighten on the active entry. Colour or a pill on a volume count would give a total the weight of something that needs action.

### Semantic Colors
Colors are muted but distinct, avoiding harsh neon tones while maintaining semantic meaning.
- **Text:** `--fg-strong` (`#f2f4f7`, titles and selected entries), `--fg` (`#e7eaee`), `--fg-soft` (`#c9d0d9`, rail entries and field labels), `--fg-dim` (`#a3acb8`), `--fg-mute` (`#8b95a3`)
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
- **Radii:** `10px` (`--radius-panel`) for panels and cards, `8px` (`--radius`) for stat tiles, segmented controls and selects, `6–7px` for buttons, inputs and segments, `4px` for badges, and fully round for filter chips, pills and status dots.
- **Density:** Padding remains tight (e.g., `12px 16px` for headers, `8px` gaps), but text is allowed to breathe more than in the legacy terminal theme.
- **Animation:** Minimal, purposeful motion. Used primarily for loading indicators (e.g., `pulse-skeleton 1.5s infinite ease-in-out`).

## Mechanism-specific sections

### Expandable Rows
Data tables use expandable rows (`.row.expanded`). When expanded:
- The row background shifts to the accent wash (`--accent-wash`, `rgba(122, 167, 255, 0.1)`).
- The expanded detail view uses `--bg-well` with a 2-column layout (main content + side metadata).
- Tasks are the exception: the selected task row takes the same wash, and its detail opens in a side pane on `--bg-elev` with tabs instead of under the row. The pane's Overview gains its side metadata column only when the pane is wide enough.
- Collapsible field carets rotate `-90deg` for clear state indication.

## Agent Signature
gemini

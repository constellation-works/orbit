---
type: design
summary: "Spec: Canon Refined Theme"
tags: ["user-interface"]
last_validated: 2026-09-12
---

# Spec: Canon Refined Theme

This document defines the formal design tokens and visual language for the Orbit User Interface (Canon Refined aesthetic), superseding the deprecated Trading Terminal theme.

## Why This Exists

As Orbit matures, the extreme constraints of the "Trading Terminal" aesthetic (pitch black, pure monospace, sharp 0px corners) proved too rigid for complex, hierarchical data presentation like nested task plans, conversational review threads, and rich telemetry. The "Canon Refined" theme provides a balanced, modern, high-density dashboard language that maintains a "pro-tool" feel while adopting established UI affordances (subtle rounding, sans-serif readability, softer semantic colors).

## Design Tokens

### Background & Elevation
The theme uses a layered dark mode, relying on subtle lightness shifts rather than shadows.
- `--bg`: `#0a0a0b` (Base canvas)
- `--bg-elev`: `#111114` (Cards, panels, buttons)
- `--bg-rail`: `#0c0c0f` (Navigation rail)
- `--bg-sunk`: `#0d0d10` (Sticky group headings and wells)
- `--bg-selected`: `#161b28` (The selected rail entry)

### Borders
Borders delineate structure without heavy contrast.
- `--border`: `#26262d` (Panel and control edges)
- `--hair`: `#1b1b21` (Dividers inside panels and controls)
- Focused inputs use the `--accent` border; there is no dedicated `--border-strong` token.

### Typography
- **Sans-serif (Primary):** `Geist`, self-hosted, used for prose, titles, and general UI text.
- **Monospace (Secondary/Data):** `Geist Mono`, self-hosted, used for IDs, metrics, timestamps, and code snippets.
- **Base Size:** `14px` with `1.5` line height.

### Semantic Colors
Colors are muted but distinct, avoiding harsh neon tones while maintaining semantic meaning.
- **Text:** `--fg` (`#ededf0`), `--fg-dim` (`#8f8f99`), `--fg-mute` (`#6b6b75`)
- **Accent (Blue):** `--accent` (`#8ab3ff`)
- **Success/Done (Green):** `--status-done` (`#5ad8a0`)
- **In-Progress (Teal):** `--status-in-progress` (`#5cc8de`)
- **Review (Purple):** `--status-review` (`#d39bff`)
- **Warning/Proposed (Amber):** `--status-proposed` (`#f2b35e`)
- **Error/Blocked (Red):** `--status-blocked` (`#ff8a80`)

Status colours are lighter than the Tailwind 500 steps they replaced so a 7px dot stays distinct on the near-black canvas; each is paired with a word, so colour is never the only signal.

### Structural Rules
- **Radii:** `12px` for panels, `8px` (`--radius`) for cards and segmented controls, `6–7px` for buttons, inputs and selects, and fully round for filter chips and status dots.
- **Density:** Padding remains tight (e.g., `12px 16px` for headers, `8px` gaps), but text is allowed to breathe more than in the legacy terminal theme.
- **Animation:** Minimal, purposeful motion. Used primarily for loading indicators (e.g., `pulse-skeleton 1.5s infinite ease-in-out`).

## Mechanism-specific sections

### Expandable Rows
Data tables use expandable rows (`.row.expanded`). When expanded:
- The row background shifts to an accent wash (`rgba(110, 159, 255, 0.05)`).
- The expanded detail view uses `#050505` with a 2-column layout (main content + side metadata).
- Collapsible field carets rotate `-90deg` for clear state indication.

## Agent Signature
gemini

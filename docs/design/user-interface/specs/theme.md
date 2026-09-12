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
- `--bg`: `#000000` (Base canvas)
- `--bg-elev`: `#0a0a0a` (Cards, panels, buttons)
- Expanded task details use `#050505`; there is no dedicated `--bg-sunk` token in the live stylesheet.

### Borders
Borders delineate structure without heavy contrast.
- `--border`: `#2a2a2e` (Panel and control edges)
- `--hair`: `#17171a` (Dividers inside panels and controls)
- Focused inputs use the `--accent` border; there is no dedicated `--border-strong` token.

### Typography
- **Sans-serif (Primary):** `Inter` with `Geist Sans` fallback, used for prose, titles, and general UI text.
- **Monospace (Secondary/Data):** `Geist Mono` with `JetBrains Mono` fallback, used for IDs, metrics, timestamps, and code snippets.
- **Base Size:** `14px` with `1.5` line height.

### Semantic Colors
Colors are muted but distinct, avoiding harsh neon tones while maintaining semantic meaning.
- **Text:** `--fg` (`#dcdcdc`), `--fg-dim` (`#71717a`)
- **Accent (Blue):** `--accent` (`#6e9fff`)
- **Success/Done (Green):** `--status-done` (`#10b981`)
- **In-Progress (Teal):** `--status-in-progress` (`#06b6d4`)
- **Review (Purple):** `--status-review` (`#d946ef`)
- **Warning/Proposed (Amber):** `--status-proposed` (`#f59e0b`)
- **Error/Blocked (Red):** `--status-blocked` (`#ef4444`)

### Structural Rules
- **Radii:** The stylesheet uses `2px` for many controls, `4px` for small components, and `6px` for panel-like containers.
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

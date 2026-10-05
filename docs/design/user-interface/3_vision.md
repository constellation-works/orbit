---
summary: "User Interface — Vision"
type: design
title: "User Interface — Vision"
owner: gemini
last_updated: 2026-04-30
last_validated: 2026-09-12
status: Draft
feature: user-interface
doc_role: vision
tags: ["user-interface"]
---

# User Interface — Vision

This document scopes forward-looking UI work beyond the current dashboard: richer interactivity, careful motion, and a reusable design system that can span local runtime views and public project pages.

## 1. Open Questions

1. **Framework adoption:** When dashboard state outgrows vanilla HTML/JS, is Preact, Svelte, or another small runtime worth the build complexity?
2. **Motion:** Can the mark or status surfaces respond to real telemetry without becoming decorative noise?
3. **Component sharing:** How do dashboard and static-site UI share tokens and components without a heavy Node.js pipeline?

## 2. Prior Work

### Terminal and TUI Tools
- **k9s and htop:** Dense, keyboard-driven system views that validate compact tables and high-contrast status.
- **Bloomberg Terminal:** A proof point for high-stakes density, fast recognition, and tolerated learning curves.

### Modern Pro-Tools
- **Linear and Vercel:** Benchmarks for quiet dark surfaces, precise typography, and keyboard-friendly interaction.

## 3. What May Be Distinctive

Orbit can be distinctive by showing agent work as inspectable operations rather than hiding it behind chat. Graphs, traces, policy denials, scoreboards, and live logs should stay visible enough for engineering judgment while Canon Refined keeps the surface controlled [T20260427-29].

## 4. References

- Orbit-internal: [Canon Refined Theme Spec](./specs/theme.md)
- External: k9s, htop, Bloomberg Terminal, Linear, and Vercel remain comparison points for density, polish, and operator trust.

## Task References

- [T20260427-29] introduced the Canon Refined UI direction.
- [T20260430-24] tightened this vision doc around open questions and references.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

---
title: Terminal Interface — Vision
owner: claude
last_updated: 2026-08-01
last_validated: 2026-09-20
status: Accepted
feature: terminal-interface
doc_role: vision
type: design
summary: "Open questions for orbit's terminal surface — TUI vs. composable CLI, agent-readable output, progress under concurrency — and the prior art the house style borrows from."
tags: [terminal-interface]
paths: ["crates/orbit-cli/src/output/**"]
related_features: [terminal-interface, user-interface, resident-orchestrator]
related_artifacts: []
---

# Terminal Interface — Vision

This document scopes questions the current specs deliberately do not answer: whether Orbit's terminal surface should stay a composable CLI or grow an interactive layer, how output should serve agent readers as distinct from human and script readers, and how live work should be represented when several runs are in flight. The rendering rules already decided are in [./specs/](./specs/) and are not restated here.

## 1. Open Questions

1. **Composable CLI or TUI?** Table rendering has so far been pushed toward adapting to the terminal rather than toward owning it — [T20260411-0335] made tables shrink and truncate for narrow screens rather than introducing a view that controls the screen. A hypothetical `orbit run status` loop is a worse `k9s`. But a TUI is a second surface with its own state, keybindings, and failure modes, and it cannot be piped. Is the right answer a genuinely live subcommand (`orbit watch`) that owns the alternate screen, leaving every other command line-oriented — or does an interactive layer belong in the dashboard, which already exists?

2. **Is an agent a third audience?** [Terminal Output Is a Rendering of a Structured Payload](./4_decisions.md#terminal-output-is-a-rendering-of-a-structured-payload) resolves output for a human and for a script. An agent reading `orbit task list` through a shell tool is neither: it wants the density of the table (tokens are the budget) but the unambiguity of JSON, and it fails differently — silently misparsing rather than erroring. A fourth mode is the obvious move and probably the wrong one. Does `ndjson` already cover this, or is the real answer that agents should use the MCP surface and never the CLI?

3. **Progress under concurrency.** Orbit dispatches waves of concurrent runs. A single spinner misrepresents that, and N spinners fight over the cursor. What does honest progress look like for work that is parallel, long-lived, and mostly happening on another machine — and does any of it survive `--format json`?

4. **Where does stderr's contract live?** The specs govern stdout. Warnings, progress, and diagnostics go to stderr, which has no shape at all today. Does it get the same payload treatment, or is unstructured stderr the correct answer for a tool whose stdout is the contract?

5. **Shared vocabulary with the dashboard.** [user-interface](../user-interface/1_overview.md) has its own status palette under Canon Refined. A `blocked` task should not be red in one surface and amber in the other. Is there a single source of status semantics both render from, without coupling a Rust CLI to dashboard CSS?

6. **Enforcement.** [2_design.md §9](./2_design.md#9-concerns--honest-limitations) notes nothing prevents a new bordered table from being added. Is a clippy lint or a grep-based CI check worth its false-positive rate, or is review the honest mechanism?

## 2. Prior Work

### Line-Oriented CLIs
- **`gh` and `kubectl`:** the borderless-header-plus-columns convention this house style adopts, including `gh`'s behavior of dropping decoration entirely when not a TTY. `kubectl -o` is the model for a mode flag that spans every subcommand rather than being redeclared per command.
- **`rg` and `fd`:** evidence that respecting `NO_COLOR` and TTY detection by default is expected, not a courtesy.
- **`jq`:** the counter-argument to a bespoke agent mode — if output is clean JSON, the composition tool already exists.

### Interactive Terminal Tools
- **`k9s` and `htop`:** dense, keyboard-driven, alternate-screen views. Cited in [user-interface/3_vision.md](../user-interface/3_vision.md) as dashboard precedent; here they are the reference for what an `orbit watch` would have to be as good as to justify existing.
- **`lazygit`:** a TUI over a CLI whose commands remain fully usable standalone — the shape question 1 is really asking about.

### Structured-Output Systems
- **PowerShell:** objects through the pipeline, rendered only at the end. [Terminal Output Is a Rendering of a Structured Payload](./4_decisions.md#terminal-output-is-a-rendering-of-a-structured-payload) is a weaker version of the same idea, constrained to a Unix pipe carrying bytes.
- **`nushell`:** structured data as the native currency, with rendering as a terminal concern. Useful mainly as evidence that the payload-first split is not exotic.

## 3. What May Be Distinctive

Orbit's terminal output is read by humans, scripts, and the agents Orbit itself dispatches — often the same command, in the same session, by all three. Most CLIs serve two audiences and treat the third as an accident. Designing for the case where the reader might be a model is the part with no settled prior art, and it is where a wrong choice is expensive: a human notices a broken column, a script errors on bad JSON, and an agent quietly proceeds on a misread value. That asymmetry, more than density or polish, is what should drive this surface.

## 4. References

- Orbit-internal: [./specs/table-rendering.md](./specs/table-rendering.md), [./specs/color-and-styling.md](./specs/color-and-styling.md), [./specs/output-modes.md](./specs/output-modes.md), [user-interface/3_vision.md](../user-interface/3_vision.md), [mcp-bridge/1_overview.md](../mcp-bridge/1_overview.md)
- External: `gh`, `kubectl`, `rg`, `jq`, `k9s`, `lazygit`, PowerShell, and `nushell` as comparison points; the `NO_COLOR` convention (no-color.org) as the baseline contract.

## Task References

- [T20260411-0335] — introduced the table arrangement whose limits motivate questions 1 and 3.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

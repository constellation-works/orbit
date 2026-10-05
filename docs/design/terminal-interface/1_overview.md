---
title: Terminal Interface — Overview
owner: claude
last_updated: 2026-08-02
last_validated: 2026-09-19
status: Accepted
feature: terminal-interface
doc_role: overview
type: design
summary: "House style for orbit-cli terminal output — machine-readable first, borderless single-line tables, semantic color resolved at the sink."
tags: [terminal-interface]
paths: ["crates/orbit-cli/src/output/**", "crates/orbit-cli/src/command/**"]
related_features: [terminal-interface, user-interface]
related_artifacts: []
---

# Terminal Interface — Overview

Terminal Interface owns what `orbit` writes to stdout and stderr: table layout, color, truncation, output modes, progress, and errors. It is the CLI counterpart to [user-interface](../user-interface/1_overview.md), which owns the web dashboard — the two surfaces share operator vocabulary and status semantics but no code, no tokens, and no rendering assumptions. Its governing claim is that terminal output has two audiences with opposed needs, and that serving both is a matter of resolving which one is present rather than compromising between them.

## 1. Motivation

A CLI's output is read by a human scanning for a status and by a pipeline extracting a field, and those readers want different bytes. The human wants alignment, color, and a screenful; the pipeline wants one record per line and no escape sequences. Historically Orbit sent both readers the same bytes, chosen for the human; the terminal-interface work now resolves the rendering for the detected sink.

The concrete failures were small and additive. `orbit tool list` drew box rules and wrapped long cells across three or four lines, so a row was not a line and `grep` returned fragments ([T20260411-0335] introduced the full-width dynamic arrangement that caused the wrapping); the shared list renderer is now borderless and one line per record [ORB-10567]. `orbit audit list` formerly used literal padding widths in a format string; it now builds a computed-width table. `output/sink.rs` resolves terminal state and color once, then applies the answer to both table and line renderers. Commands return shared payloads for central rendering; the legacy `--json`/`--ops` flags remain on their existing command argument types, while the global `--format` surface supplies the common modes.

None of these is severe alone. Together they mean output correctness is a property each command has or lacks individually, which is the thing this feature exists to stop.

## 2. Core Concepts

- **Payload** — the structured record a command produces. The contract; both renderings derive from it [Terminal Output Is a Rendering of a Structured Payload](./4_decisions.md#terminal-output-is-a-rendering-of-a-structured-payload).
- **Renderer** — the layer that projects a payload into bytes. The only code that knows about terminals, width, or ANSI.
- **Output mode** — `auto` | `table` | `json` | `ndjson`. Resolved centrally from flags and TTY state, never per command.
- **Role** — a semantic color token (`ok`, `warn`, `error`, `active`, `muted`, `neutral`). Commands tag values with roles; only the renderer maps roles to color [One Semantic Color Vocabulary, Gated at the Sink](./4_decisions.md#one-semantic-color-vocabulary-gated-at-the-sink).
- **Sink** — the resolved stdout target plus its capabilities (is it a TTY, how wide, may it carry ANSI). Every environment question is answered here once.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Table construction, preset, arrangement | [crates/orbit-cli/src/output/table.rs](../../../crates/orbit-cli/src/output/table.rs) | [T20260411-0335] |
| Status and priority color vocabulary | [crates/orbit-cli/src/output/color.rs](../../../crates/orbit-cli/src/output/color.rs) | [T20260427-43] |
| Structured output and error payloads | [crates/orbit-cli/src/output/json.rs](../../../crates/orbit-cli/src/output/json.rs) | [ORB-10356] |
| Hand-padded audit line output | [crates/orbit-cli/src/command/audit/support.rs](../../../crates/orbit-cli/src/command/audit/support.rs) | [ORB-10228] |
| Per-command rendering (current, non-conforming) | [crates/orbit-cli/src/command/](../../../crates/orbit-cli/src/command/) | [ORB-00279] |
| Target table contract | [./specs/table-rendering.md](./specs/table-rendering.md) | [Borderless Tables With Truncate-to-Width Rows](./4_decisions.md#borderless-tables-with-truncate-to-width-rows) |
| Target color contract | [./specs/color-and-styling.md](./specs/color-and-styling.md) | [One Semantic Color Vocabulary, Gated at the Sink](./4_decisions.md#one-semantic-color-vocabulary-gated-at-the-sink) |
| Target mode-resolution contract | [./specs/output-modes.md](./specs/output-modes.md) | [Terminal Output Is a Rendering of a Structured Payload](./4_decisions.md#terminal-output-is-a-rendering-of-a-structured-payload) |

## Task References

- [T20260411-0335] — introduced the dynamic full-width table arrangement for narrow terminals.
- [T20260427-43] — added a `friction` arm to both halves of the duplicated color vocabulary.
- [ORB-00279] — flattened the `orbit-cli` command tree into the current per-command layout.
- [ORB-10228] — added trusted MCP session context to the audit event JSON payload, but not to the printed line.
- [ORB-10356] — made `OrbitError` `#[non_exhaustive]`, adding the `internal_error` catch-all to the payload's `code` discriminator.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

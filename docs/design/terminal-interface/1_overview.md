---
title: Terminal Interface — Overview
owner: claude
last_updated: 2026-11-08
last_validated: 2026-11-08
status: Implemented
stage: shipped
next: "—"
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

The concrete failures were small and additive. `orbit tool list` drew box rules and wrapped long cells across three or four lines, so a row was not a line and `grep` returned fragments ([T20260411-0335] introduced the full-width dynamic arrangement that caused the wrapping); the shared list renderer is now borderless and one line per record [ORB-10567]. `orbit audit list` formerly used literal padding widths in a format string; it now builds a computed-width table. `output/sink.rs` resolves terminal state and color once, then applies the answer to both table and line renderers. Commands return shared payloads for central rendering; the global `--format` surface supplies the common modes, and the global `--json` shorthand selects pretty JSON across the assembled CLI. Older command-specific flags such as `--ops` remain where their argument types define them.

None of these is severe alone. Together they mean output correctness is a property each command has or lacks individually, which is the thing this feature exists to stop.

## 2. Core Concepts

- **Payload** — the structured record a command produces. The contract; both renderings derive from it [Terminal Output Is a Rendering of a Structured Payload](./4_decisions.md#terminal-output-is-a-rendering-of-a-structured-payload).
- **Renderer** — the layer that projects a payload into bytes. The only code that knows about terminals, width, or ANSI.
- **Output mode** — `auto` | `table` | `plain` | `json` | `ndjson`. Resolved centrally from flags and TTY state, never per command.
- **Role** — a semantic color token (`ok`, `warn`, `error`, `active`, `muted`, `neutral`). Commands tag values with roles; only the renderer maps roles to color [One Semantic Color Vocabulary, Gated at the Sink](./4_decisions.md#one-semantic-color-vocabulary-gated-at-the-sink).
- **Sink** — the resolved stdout target plus its capabilities (is it a TTY, how wide, may it carry ANSI). Every environment question is answered here once.

The main rendering migration is done: borderless tables [ORB-10567], semantic roles [ORB-10568], sink resolution [ORB-10569], gating and stderr errors [ORB-10570], output fixtures [ORB-10571], and command payloads [ORB-10586]. Record-output follow-ups [ORB-11596], doctor repair counts [ORB-11597], and workspace initialization [ORB-11622] are also done. Filtered-value neutrality is done [ORB-21574]. Full detail recovery for truncated views is done [ORB-21575]: doctor records and tool parameters have single-record selectors, and migration labels are fixed. Non-record stdout trailers are also fixed [ORB-21576], landed in base commit `3040f8025`. With detail recovery delivered here, the committed scope is complete; the accepted design contract is unchanged.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Table construction, preset, arrangement | [crates/orbit-cli/src/output/table.rs](../../../crates/orbit-cli/src/output/table.rs) | Done: [ORB-10567] (replaces [T20260411-0335]) |
| Status and priority color vocabulary | [crates/orbit-cli/src/output/color.rs](../../../crates/orbit-cli/src/output/color.rs) | Done: [ORB-10568], [ORB-10570] (replaces [T20260427-43]) |
| Structured output and error payloads | [crates/orbit-cli/src/output/json.rs](../../../crates/orbit-cli/src/output/json.rs) | [ORB-10356] |
| Audit list table and JSON payload | [crates/orbit-cli/src/command/audit/support.rs](../../../crates/orbit-cli/src/command/audit/support.rs) | [ORB-10228] |
| Shared payload construction and rendering | [crates/orbit-cli/src/command/](../../../crates/orbit-cli/src/command/) | Done: [ORB-10586], [ORB-11596] (layout from [ORB-00279]) |
| Table contract; full-detail recovery is done [ORB-21575] | [./specs/table-rendering.md](./specs/table-rendering.md) | [Borderless Tables With Truncate-to-Width Rows](./4_decisions.md#borderless-tables-with-truncate-to-width-rows) |
| Color contract; filtered neutrality is done [ORB-21574] | [./specs/color-and-styling.md](./specs/color-and-styling.md) | [One Semantic Color Vocabulary, Gated at the Sink](./4_decisions.md#one-semantic-color-vocabulary-gated-at-the-sink) |
| Mode contract; stdout trailers are done [ORB-21576] | [./specs/output-modes.md](./specs/output-modes.md) | [Terminal Output Is a Rendering of a Structured Payload](./4_decisions.md#terminal-output-is-a-rendering-of-a-structured-payload) |

## Task References

- [ORB-00279] — flattened the `orbit-cli` command tree into the current per-command layout.
- [ORB-10228] — added trusted MCP session context to the audit event JSON payload, but not to the printed line.
- [ORB-10356] — Make OrbitError non-exhaustive and add the internal-error JSON catch-all.
- [ORB-10567] — Replace bordered wrapping tables with borderless one-line rows and column metadata.
- [ORB-10568] — Consolidate CLI color helpers into a single semantic role mapping.
- [ORB-10569] — Introduce the output sink and global format selection.
- [ORB-10570] — Gate color and width at the sink, convert audit tables, and move errors to stderr.
- [ORB-10571] — Add the first CLI output regression fixtures and goldens.
- [ORB-10586] — Convert command bodies to payload returns and give the shared renderer stdout ownership.
- [ORB-11596] — Convert remaining record-output commands onto shared payload rendering.
- [ORB-11597] — Move doctor repair notices to stderr and include repair counts in structured result rows.
- [ORB-11622] — Convert workspace initialization to shared payload rendering.
- [ORB-21574] — Make filtered table cells neutral while keeping their columns visible.
- [ORB-21575] — Add full per-record detail selectors for diagnostic messages and tool parameters; keep migration labels whole.
- [ORB-21576] — Move non-record health/status trailers to stderr in every mode.
- [T20260411-0335] — Introduce the earlier dynamic full-width table arrangement.
- [T20260427-43] — Extend the earlier duplicated color vocabulary for friction status.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

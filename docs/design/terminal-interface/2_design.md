---
title: Terminal Interface — Design
owner: claude
last_updated: 2026-08-02
last_validated: 2026-09-20
status: Accepted
feature: terminal-interface
doc_role: design
type: design
summary: "Current orbit-cli output implementation — borderless single-line tables sized by the sink, one semantic color mapping gated in one place, errors on stderr — and where it still diverges from the specs."
tags: [terminal-interface]
paths: ["crates/orbit-cli/src/output/**", "crates/orbit-cli/src/command/**"]
related_features: [terminal-interface]
related_artifacts: []
---

# Terminal Interface — Design

This document describes what `orbit-cli` renders today. It is deliberately a record of the current implementation, not of the target: the target is prescribed by [./specs/](./specs/) and decided in [./4_decisions.md](./4_decisions.md). Where current behavior diverges from a spec, this doc says so and names the spec, so a reviewer can tell an intentional gap from a regression. Web dashboard rendering is out of scope and lives in [user-interface/2_design.md](../user-interface/2_design.md).

## 1. The Output Module

`crates/orbit-cli/src/output/` has seven production modules. `table.rs` owns list rendering, `color.rs` maps domain values to roles, `payload.rs` carries records and their human view, `render.rs` projects that view into bytes, `json.rs` serializes payloads and error envelopes, `sink.rs` resolves the invocation's rendering answers, and `pipe.rs` turns a closed stdout into a silent exit. Command bodies return payloads; `render.rs` is the shared renderer, while `table.rs` retains the layout policy (§2).

`payload.rs` is the normal-output contract and `render.rs` owns its projection. `json.rs` still owns `error_payload`, which projects an `OrbitError` into `{error, code}` plus optional `did_you_mean`, `artifact_origin`, and task-bundle corruption fields. Two caveats remain: the `code` discriminator gained a `_ => "internal_error"` catch-all when `OrbitError` became `#[non_exhaustive]` [ORB-10356], so an unmapped variant degrades silently rather than failing to compile; and a `RemoteTool` error returns the remote payload verbatim, bypassing the shape entirely, so a consumer cannot assume `code` is present.

## 2. Table Construction

`output/table.rs` owns a `Table` type that buffers rows and renders them itself; `comfy_table` is an implementation detail it never hands out. `Table::add_row` is the only row constructor and caps every row at `Row::max_height(1)`, so the unbounded path that made a row span three or four lines is unreachable from a command module [ORB-10567]. The 27 command modules construct either `build_table(&headers)` (all columns plain text) or `Table::new(vec![Column…])` when a column is an identifier, a number, or a path.

Rendering applies the `NOTHING` preset with `ContentArrangement::Disabled`, two-space right padding on every column but the last, and a dim header row. Widths are computed from the result set: natural widths first, then flexible columns shrink widest-first to a floor of 8, then flexible columns drop from the right with a notice on stderr. Fixed columns never move. Overflow truncates with `…` — tail truncation is `comfy_table`'s, middle truncation (`Column::path`) is applied before the grid sees the cell.

Two selection rules run before layout: a column whose value is identical in every row of a result set is suppressed unless the caller marked it `filtered` (the user filtered on it) or the view opted out with `keep_all_columns`, and a result set with no records prints its empty-state line to stderr, leaving stdout empty. Satisfies [./specs/table-rendering.md](./specs/table-rendering.md) §§1–6 [Borderless Tables With Truncate-to-Width Rows](./4_decisions.md#borderless-tables-with-truncate-to-width-rows). Width and styling are both read from the sink in `Table::emit` [ORB-10570], so a zero-width sink truncates nothing and a sink that disallows color renders the bytes a file would get. The plain form now suppresses the header and disables truncation, while [./references/detail-commands.md](./references/detail-commands.md) records which truncatable columns have detail commands and which views still lack one.

## 3. Line Output

The last hand-padded list is gone. `orbit audit list` used to print each event through `print_audit_event_line` in `command/audit/support.rs`, a single `println!` with the format string `"[{}] {:<8} {:<6} {}:{:<20} {}ms"` — literal widths that held only while every value fit, no header, and a left-aligned duration carrying its unit per row. It now builds a `Table` with computed widths, a header, a `filtered` flag per filterable column, and a right-aligned `DURATION (ms)` [ORB-10570]. Satisfies [./specs/table-rendering.md](./specs/table-rendering.md) §2 and §3.

30 modules under `command/` still call `println!` directly for some part of their output — detail views, field labels, confirmations — so line output remains a pattern rather than an isolated case. Record output is nevertheless centralized: command payloads go through `output::render`, while the remaining prose paths use `output::color`, whose `colored` backend is overridden from the sink at startup.

The audit view still demonstrates the difference between the two renderings. [ORB-10228] added trusted session-context fields (`workspace_id`, `caller_machine_id`, `transport`, `mcp_call_id`) to `audit_event_to_json`; the human table does not expose those fields, while the shared payload keeps them available to JSON consumers.

## 4. One Color Vocabulary

`color.rs` holds a single `role_for(Domain, &str) -> Role` table covering task status, priority, task type, job state, doctor status, and audit status together, and two renderings of a tagged value: `cell(value, tag)` for a table row and `text(value, tag)` for a line. A `tag` is either a `Role` the call site names outright — `text("Workspace healthy.", Role::Ok)` — or the `Domain` the value came from, which the table resolves. Unmapped values are `Neutral`, never a panic.

The eight per-domain wrappers this replaced (`status_color`/`_cell`, `priority_color`/`_cell`, `job_state_color`/`_cell`, `doctor_status_color_cell`, `task_type_color_cell`) are gone, and with them the two-edit cost visible in the history — [T20260427-43] added a `friction` arm to two functions, [ORB-10202] removed it from two — and the `backlog` drift between the pair. Adding a status is now one line in one table. Satisfies [./specs/color-and-styling.md](./specs/color-and-styling.md) §1 [One Semantic Color Vocabulary, Gated at the Sink](./4_decisions.md#one-semantic-color-vocabulary-gated-at-the-sink).

**Still diverges on §3's palette depth.** `comfy_table` renders `Color::Green` through `crossterm` as `\e[38;5;10m`, a 256-color code, while `colored` renders the same role as `\e[32m`. The spec asks for 16-color ANSI only; the two backends therefore still disagree about the *shade*, though no longer about *whether* to emit. **Still diverges on §3's uniform-filter rule**: a column the caller filtered on stays on screen but is still painted, in every list command.

## 5. Sink Resolution

`output/sink.rs` resolves `is_tty`, `width`, `color_allowed`, and the output mode once per invocation, from `main.rs`, ahead of dispatch [ORB-10569]. It is the only place in the crate that queries terminal state — `COLUMNS`, `TIOCGWINSZ`, `NO_COLOR`, `CLICOLOR_FORCE`, `IsTerminal` — enforced by `scripts/check-terminal-state-guard.sh`, which carries no grandfathered exception: `command/log/tail.rs` colorizes from the sink the renderer hands its stream, not from a check of its own [ORB-10570], [ORB-10586].

`main` resolves the sink, calls `sink.apply_color_policy()` (which overrides `colored`'s own detection process-wide), and passes it to `output::render::emit` — the single consumer. There is no `OnceLock`: [ORB-10586] threaded the payload return through `Execute::execute`, which made the sink available as a renderer parameter, and removed the process-global `sink::install`/`sink::active` path. §9 records the remaining limitations.

Both backends are now told, never asked: `Table::render_at` calls `enforce_styling()` or `force_no_tty()` from `sink.color_allowed()`, and `colored` is overridden at startup. `NO_COLOR=1` on a terminal therefore produces byte-identical output to a redirect on both a table-rendering and a line-rendering command, asserted in `crates/orbit-cli/src/output/tests/gating.rs`. Table width comes from `sink.truncate_width()`, so a zero-width sink truncates nothing. Satisfies [./specs/output-modes.md](./specs/output-modes.md) §1 and [./specs/color-and-styling.md](./specs/color-and-styling.md) §2 [Terminal Output Is a Rendering of a Structured Payload](./4_decisions.md#terminal-output-is-a-rendering-of-a-structured-payload), [One Semantic Color Vocabulary, Gated at the Sink](./4_decisions.md#one-semantic-color-vocabulary-gated-at-the-sink).

**Mode now drives the success path too.** A command returns a payload — a JSON document plus the blocks of its human view — and `output::render::emit` projects it into `table`, plain, `json`, or `ndjson` [ORB-10586]. `--format` and `ORBIT_FORMAT` are live on every list and detail command, `auto` on a pipe produces the plain form, and `ndjson` streams one record per line with a flush per record. Satisfies [./specs/output-modes.md](./specs/output-modes.md) §2–§3. The commands that still return `CommandOutput::Silent` are the mutations and confirmations, which have no record stream to project.

## 6. Per-Command Structured Output

Legacy `--json`/`--ops` flags remain declared independently where they are accepted (83 `pub json: bool` fields under `crates/orbit-cli/src/command/`), but main now reads those flags as a compatibility mode input. Commands build a `Payload` and the shared renderer chooses the human, JSON, or NDJSON projection.

The shared path keeps the JSON document and human view together. `orbit tool list` still derives a human `REQUIRED INPUT` summary from the parameter data, but it is rendered from the same collected records as the JSON document. The global `--format json|ndjson` modes are available alongside the legacy booleans; NDJSON emits one complete record per line. The remaining compatibility exceptions are documented in §§7 and 9.

A global `--format auto|table|json|ndjson` is accepted on every command that does not already own a `--format` — `audit export` and `hook pretooluse` do, with unrelated value types, and keep theirs. `main` extracts the deepest global value, resolves it once with the sink, and `output::render::emit` consumes it for successful payloads. On the failure path it is also load-bearing: `--format json` on a command with no `--json` flag of its own still produces a machine-readable error payload [ORB-10570].

## 7. Empty States and Errors

Table-backed empty results print their configured empty-state line to stderr and leave stdout empty, including `command/policy/list.rs`. The hidden legacy `command/task/artifacts.rs` path still prints a human sentence for task artifacts.

Errors are routed through `main.rs::print_error`, which emits the JSON error payload when the command declared a JSON preference *or* the sink resolved a JSON mode, and a plain message otherwise. **Both go to stderr, in every mode** [ORB-10570]. This is a breaking change for any script that parsed the error object off stdout; the exit code (`1`, or `2` for a clap usage error) was already the reliable signal and is unchanged.

A closed stdout is not an error. `output/pipe.rs` installs a panic hook that turns the `failed printing to stdout: … (os error 32)` panic `println!` raises into a silent `exit(0)`, and `log tail` maps a `BrokenPipe` write error to `Ok`. `orbit audit list --limit 20000 | head -1` (2.5 MB, well past the pipe buffer) exits 0 with empty stderr. Satisfies [./specs/output-modes.md](./specs/output-modes.md) §5.

## 8. Test Coverage of Output

There are no output snapshots. `crates/orbit-cli/src/snapshots/` holds a single file, `audit_guard_event_json_shapes.json`, covering audit event JSON shape; `crates/orbit-cli/tests/snapshots/` holds one more, `mcp_tools_list.json`, covering the MCP tool listing. Both assert on JSON.

The first rendering assertions arrived with the borderless migration [ORB-10567] and are written as behavior, not golden files. `crates/orbit-cli/src/output/tests/table.rs` renders at pinned widths — passed as an argument rather than read from `COLUMNS`, so a `--nocapture` run cannot change the geometry — and asserts line count, gutter, truncation, alignment, column suppression, and column dropping. `crates/orbit-cli/tests/table_rendering.rs` runs the binary and asserts that an *N*-record `orbit tool list --all` and `orbit task list` are *N* body lines under one header with no box glyphs, and that a zero-result list leaves stdout empty.

`crates/orbit-cli/src/output/tests/gating.rs` asserts ANSI emission [ORB-10570]: that a `NO_COLOR` terminal and a redirect render byte-identically on both a table and a log line, that a terminal *without* `NO_COLOR` renders escapes through both backends (so the equality tests cannot pass vacuously), that a zero-width sink truncates nothing, and that progress is refused off a terminal and in `json`/`ndjson`. Sinks are built with `OutputSink::resolve`, never `from_process`, because `make ci` runs without a TTY. The one test that flips `colored`'s process-global override serializes on a module mutex and restores detection on drop.

## 9. Concerns & Honest Limitations

**The specs and implementation now share the main rendering path.** Remaining differences are explicit compatibility cases: mutation commands may still return `Silent`, and the two styling backends can choose different shades for the same semantic role. Reading only `specs/` still omits those implementation details.

**The payload refactor was broad.** `Execute::execute` now returns `CommandOut` (`Result<CommandOutput, OrbitError>`), and `main` hands the resulting payload to `output::render`. `CommandOutput::Silent` remains for mutations and confirmations that have no record stream, so the migration has a deliberately small compatibility boundary rather than a second record-rendering path.

**Terminal width detection needed no new dependency.** The sink asks `TIOCGWINSZ` through the `libc` dependency `orbit-cli` already carries on unix, after `COLUMNS`. Non-unix has no query and falls back to `COLUMNS` alone. `comfy-table` no longer resolves width for the tables it draws — `Table::render_at` passes an absolute constraint per column — so the 0 fallback ("do not truncate") reaches a real consumer and is asserted against one.

**The role vocabulary is lossy on purpose and may be wrong.** Collapsing `in-progress`, `review`, and `proposed` into `active`/`neutral` discards distinctions operators may be reading today. No one has checked whether the current colors are load-bearing in practice.

**Accessibility is unaudited.** The palette has never had a contrast check against common terminal themes, and the specs' "color is never the sole carrier" rule is asserted, not verified.

**Little is enforced.** `scripts/check-terminal-state-guard.sh` rejects a new terminal-state query outside the sink, and `Table::add_row` makes a multi-line row unreachable. Nothing rejects a new `comfy_table::Table` in a command body, a new bare `println!` of tabular data, or a `colored` call that bypasses `output::color` — the last is currently harmless only because the process-wide override catches it.

**The sink is no longer a process global.** [ORB-10586] changed `Execute::execute` to return a payload, which gave the renderer a call site to take the sink as a parameter; `sink::install`/`sink::active` are deleted and two in-process invocations can render against different sinks. [The Output Sink Is a Process Global, Not a Renderer Parameter](./4_decisions.md#the-output-sink-is-a-process-global-not-a-renderer-parameter) recorded the global as the decision and is superseded by that change, not amended by it — its reasoning ("`Execute::execute` takes no renderer argument") stopped being true rather than being outweighed.

**A mutation command still writes its own prose.** `CommandOutput::Silent` means "no records", and the commands that return it — `task update`, `workspace init`, the confirmations — still `println!` their human report directly. That is not a second rendering path for *records*, but it does mean `--format json` on a mutation yields prose, and spec §5's "stdout carries the payload and nothing else" is not yet true of them.

**Nothing rejects a new `Silent` command that prints records.** The trait makes returning a payload the obvious thing to do, but a command that builds a table and prints it itself would still compile — `Table::emit` is `pub(crate)`, so it is reachable from `command/`.

## Task References

- [T20260411-0335] — introduced the dynamic full-width table arrangement that produces the current wrapping.
- [T20260427-43] — added a `friction` arm to both halves of the duplicated color vocabulary.
- [ORB-00279] — flattened the `orbit-cli` command tree into the current per-command module layout.
- [ORB-10202] — removed the `friction` arm from both halves when the status was retired.
- [ORB-10228] — added trusted MCP session context to the audit event JSON payload, but not to the printed line.
- [ORB-10356] — made `OrbitError` `#[non_exhaustive]`, adding the `internal_error` catch-all to the payload's `code` discriminator.
- [ORB-10569] — introduced `output/sink.rs` and the global `--format`, resolved once per invocation and not yet consumed.
- [ORB-10570] — wired the sink into color and width, retired the eight color wrappers and the hand-padded `audit list` line, and moved errors to stderr.
- [ORB-10585] — files the ADR for the process-global sink, which [ORB-10570] could not allocate.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

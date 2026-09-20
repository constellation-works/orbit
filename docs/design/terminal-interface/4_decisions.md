---
title: Terminal Interface — Decisions
owner: claude
last_updated: 2026-08-11
last_validated: 2026-09-20
status: Accepted
feature: terminal-interface
doc_role: decisions
type: design
summary: "Decision record for Orbit's terminal output surface."
tags: [terminal-interface]
paths: ["crates/orbit-cli/src/output/**"]
related_features: [terminal-interface]
related_artifacts: []
---

# Terminal Interface — Decisions

[Terminal Output Is a Rendering of a Structured Payload](#terminal-output-is-a-rendering-of-a-structured-payload), [Borderless Tables With Truncate-to-Width Rows](#borderless-tables-with-truncate-to-width-rows), and [One Semantic Color Vocabulary, Gated at the Sink](#one-semantic-color-vocabulary-gated-at-the-sink) were recorded on 2026-08-02, ahead of the implementation work that makes the code conform. That ordering is deliberate — the specs in [./specs/](./specs/) are the reviewed contract, and [2_design.md](./2_design.md) records per-mechanism where the code still diverges from it. A divergence noted there is scheduled work, not an unmade decision.

[The Output Sink Is a Process Global, Not a Renderer Parameter](#the-output-sink-is-a-process-global-not-a-renderer-parameter) is the inverse case: [ORB-10570] implemented it first, then [ORB-10586] changed the premise by threading the sink through `Execute::execute`. The earlier reasoning stays below with its supersession explained in prose; [2_design.md](./2_design.md) §9 records the current shape.

## Terminal Output Is a Rendering of a Structured Payload

**Recorded:** 2026-08-02 02:33:46.102495Z · [T20260411-0335], [ORB-10228], [ORB-10356]
**Paths:** `crates/orbit-cli/src/output/`, `crates/orbit-cli/src/command/`

### Context

Every `orbit` subcommand renders its own human output inline. The pattern repeats across `crates/orbit-cli/src/command/`: build a `serde_json::Value` when `--json` is passed, otherwise build a `comfy_table::Table` or `println!` a hand-padded line, in the same `execute()` body. Two consequences follow.

First, the two renderings drift. `orbit tool list` emits seven JSON fields but five table columns. `orbit audit list` emits a fixed-width line (`"[{}] {:<8} {:<6} {}:{:<20} {}ms"` in `command/audit/support.rs`) whose padding is a literal in a format string rather than a function of the data, so a tool name longer than 20 characters silently breaks the column the operator is scanning. The drift also accumulates unnoticed: [ORB-10228] added trusted session-context fields to `audit_event_to_json` in that same file and left the printed line untouched, so the JSON view carries provenance the human view has never shown.

Second, structured output is opt-in per command. 86 of 150 `#[derive(Args)]` structs carry a `pub json: bool` field, each declared and handled independently. The remaining 64 have no machine-readable path at all, and the flag's presence is not discoverable without reading the help for each command.

Nothing in the CLI detects whether stdout is a terminal, except `command/log/tail.rs`, which does it locally for one colorized stream. So `orbit tool list | grep` receives the same box-drawn, width-adapted, ANSI-styled output a human sees.

The one place output mode is already resolved centrally is `main.rs::print_error`, which routes an `OrbitError` through `output/json.rs::error_payload` when the command declared a JSON preference. That function is the closest thing the CLI has to an output contract, and this decision generalizes its shape to normal output.

### Decision

A command produces a structured payload; rendering is a separate layer that consumes it. Command bodies stop constructing tables and format strings.

- The payload is the contract. The human rendering is a projection of it and may only drop or reformat fields, never introduce a value the payload does not carry.
- Output mode is resolved centrally, not per command: an explicit global `--format` (`auto|table|json|ndjson`) wins; otherwise `auto` renders the table form when stdout is a TTY and the plain machine form when it is not.
- The existing per-command `--json` flags stay as accepted aliases for `--format json`. They are not removed.
- Piped output is plain: no borders, no ANSI, no width adaptation to a terminal that isn't there.

Rejected alternative: add `--json` to the 64 commands that lack it and leave the rendering inline. Rejected because it treats the symptom. The drift between the JSON and table views is caused by their being built independently in the same function; adding more independent pairs makes the invariant harder to hold, and still leaves a piped `orbit tool list` emitting box-drawing characters.

Rejected alternative: make `--format json` the default and have humans opt into tables. Rejected because it degrades the far more common interactive case to serve scripts, which can set the flag explicitly and, under this decision, get the right thing from a pipe anyway.

### Consequences

- A piped or redirected `orbit` command is parseable by `cut`/`awk` without flags, which is the property that currently fails.
- New commands get structured output for free, so the 64-command gap closes as a consequence of the refactor rather than as 64 separate changes.
- The JSON and human views cannot disagree about a field's value, because one is derived from the other.
- Cost: every command that prints must be refactored to return a payload instead of writing to stdout, and `Execute::execute` returning `Result<(), OrbitError>` no longer expresses that — the trait signature changes, touching all 154 `impl Execute` blocks.
- Cost: output becomes environment-dependent. A command run in CI (no TTY) prints differently than the same command run in a terminal, so any test or runbook that asserts on stdout must pin `--format` explicitly or it will pass locally and fail in CI.
- Cost: moving the JSON error payload from stdout to stderr is a breaking change for any existing script that parses errors off stdout, and there is no output snapshot suite that would catch the regression — `crates/orbit-cli/src/snapshots/` holds one file, covering audit event JSON shape only.

## Borderless Tables With Truncate-to-Width Rows

**Recorded:** 2026-08-02 02:33:47.634255Z · [T20260411-0335]
**Paths:** `crates/orbit-cli/src/output/table.rs`

### Context

`crates/orbit-cli/src/output/table.rs` builds every list view (21 call sites) with `presets::UTF8_BORDERS_ONLY` and `ContentArrangement::DynamicFullWidth`. Two properties follow from that pairing.

The preset draws box rules around and between the header and body. Those glyphs carry no information that column alignment does not already carry, and they are noise to `grep`, `cut`, and `awk` — the tools an operator reaches for when a list is longer than a screen.

`DynamicFullWidth` expands the table to the full terminal width and wraps overflowing cells rather than truncating them. `build_table` sets a truncation indicator, but truncation only applies to rows added through `add_single_line_row`, which caps `max_height(1)`. Only 2 of the 21 call sites use it. So in practice a long description or a multi-parameter input summary wraps to three or four lines, and a row is no longer a line. That is the actual reason the borders look necessary: once rows span variable numbers of lines, a horizontal rule is the only thing separating them. The border is compensating for the wrapping.

### Decision

Tables are borderless, and a row is exactly one line.

- Drop the box preset. A list is a dim uppercase header row, two-space gutters, and left-aligned columns. Numeric and duration columns are right-aligned so magnitudes compare vertically.
- Every row is capped to one line. Content that does not fit its column is truncated with `…`, never wrapped. `add_single_line_row` becomes the only way to add a row; the unbounded `add_row` path is not exposed.
- Truncation is a promise of a fuller view. A list command that truncates a column must have a corresponding detail command that prints the untruncated value (`orbit tool show <name>` for `orbit tool list`), and `--format json` always carries the full value regardless of terminal width.
- Columns whose value is identical in every row of a given result set are not rendered in `auto` mode.

Rejected alternative: keep borders and fix only the wrapping. Rejected because once rows are single-line the borders have nothing left to do — they were separating wrapped rows — and they remain hostile to line-oriented tooling.

Rejected alternative: an `--wide` flag that restores wrapping for full values. Rejected as a second rendering mode to maintain when `--format json` already returns the complete payload and the detail command already exists.

### Consequences

- A row is a line, so `orbit tool list | grep github` returns whole records rather than fragments of wrapped cells.
- Vertical scanning improves: aligned columns without rules put more rows on a screen and remove the glyph noise between them.
- The suppression rule removes low-entropy columns automatically — `BUILTIN` is `yes` for every row of the current `orbit tool list`, and disappears without a per-command decision.
- Cost: information leaves the list view. A long description is now only fully visible via the detail command or `--format json`, so a list becomes a navigation surface rather than a complete one, and every list command must have a detail counterpart that currently may not exist.
- Cost: the rendering depends on terminal width, so the same command truncates differently in an 80-column and a 200-column terminal. Any comparison of two captured outputs must fix `COLUMNS` or use `--format json`.

## One Semantic Color Vocabulary, Gated at the Sink

**Recorded:** 2026-08-02 02:33:49.306446Z · [T20260427-43], [ORB-10202]
**Paths:** `crates/orbit-cli/src/output/color.rs`

### Context

`crates/orbit-cli/src/output/color.rs` defines the same status vocabulary twice, in two incompatible type systems. `status_color_cell` returns a `comfy_table::Cell` for table rows; `status_color` returns a `String` styled by the `colored` crate for line output. `job_state_color_cell`/`job_state_color` and `priority_color_cell`/`priority_color` are the same duplication. Two further cell-returning functions, `doctor_status_color_cell` and `task_type_color_cell`, have no String counterpart at all — and `task_type_color_cell` applies no styling whatsoever, existing only to satisfy the cell-returning shape. Eight functions, three genuine pairs, no consistent rule about which values get a pair.

The duplication exists because the styling backend is chosen by the call site's rendering shape rather than by the meaning of the value. It means every new status needs two edits, and nothing detects when only one lands. The two-edit cost shows up in the history: [T20260427-43] added a `friction` arm to both halves, and [ORB-10202] removed it from both when the status was retired. The copies have nevertheless drifted — the string form maps `backlog` explicitly, the cell form does not.

The two backends also disagree about when to emit ANSI at all. The `colored` crate honors `NO_COLOR` and TTY detection internally. `comfy_table::Color` does not — it writes escape sequences unconditionally. So `NO_COLOR=1 orbit task list` still emits color from the table path, and a redirect captures escape sequences into the file. Nothing in the CLI sets a global override; the only TTY check in the crate is a local one in `command/log/tail.rs`.

### Decision

Color is a semantic token attached to a value's meaning, resolved once at the sink.

- A single vocabulary maps domain values to a small closed set of roles: `ok`, `warn`, `error`, `active`, `muted`, `neutral`. Commands tag a value with a role; they never name a color or call a styling crate.
- The renderer resolves role to ANSI, and is the only place either styling backend is touched. Adding a status is one edit.
- Emission is decided once, at the sink, in this precedence: `--no-color` or `NO_COLOR` (any non-empty value) disables; `--color=always` forces; otherwise color is on only when stdout is a TTY.
- Color is never the sole carrier of meaning. A status cell prints its word, and the word is legible with color stripped — this is what makes the `NO_COLOR` and piped paths correct rather than merely degraded.
- Roles apply to values, not rows. A failed row is not painted red; its status cell is.

Rejected alternative: keep the eight functions and add a test asserting the pairs agree. Rejected because it preserves the two-edit requirement, only catches drift after it is written, and says nothing about the two functions that have no pair to compare against.

Rejected alternative: drop `colored` and route all line output through `comfy-table`. Rejected because `comfy_table::Color` is the backend that ignores `NO_COLOR`, and single-value output has no table to build.

### Consequences

- `NO_COLOR=1` and redirection are honored everywhere, not only on the paths that happen to use `colored`.
- A new status or run state is defined once, and cannot appear styled in one view and unstyled in another.
- The closed role set bounds the palette, which is the precondition for a contrast audit; an open set of per-command colors could not be audited.
- Cost: a status whose role is not obvious now forces a judgment at definition time (`review` is neither `ok` nor `warn`), and mapping it to `neutral` loses a distinction the current code encodes as magenta. The vocabulary is deliberately smaller than what exists.
- Cost: the role indirection means reading the source no longer tells you what color a value prints; that requires reading the resolver too.

## The Output Sink Is a Process Global, Not a Renderer Parameter

**Recorded:** 2026-08-02 04:39:37.628708Z · [ORB-10570], [ORB-10585]
**Paths:** `crates/orbit-cli/src/output/sink.rs`, `crates/orbit-cli/src/output/table.rs`, `crates/orbit-cli/src/output/color.rs`

### Context

[Terminal Output Is a Rendering of a Structured Payload](#terminal-output-is-a-rendering-of-a-structured-payload) resolves the output sink once per invocation and requires every renderer to read it rather than re-derive terminal state. [One Semantic Color Vocabulary, Gated at the Sink](#one-semantic-color-vocabulary-gated-at-the-sink) adds that color emission must be decided in exactly one place. Both presuppose that a renderer can *reach* the sink.

It cannot, by parameter. Rendering in `orbit-cli` happens inside `Execute::execute(self, &OrbitRuntime) -> Result<(), OrbitError>`, which takes no renderer argument, across 154 `impl Execute` blocks and 98 command modules that call `println!` or `Table::print()` directly. `output/table.rs::Table::print` and `output/color.rs` are free functions those bodies call; neither has a call-site-provided sink to consult.

[Terminal Output Is a Rendering of a Structured Payload](#terminal-output-is-a-rendering-of-a-structured-payload)'s migration step 3 changes that signature so commands return payloads, and is explicitly the expensive step. [ORB-10570]'s scope — gate color and width at the sink — is independent of it and was sequenced before it precisely so that `NO_COLOR=1` stops emitting escape sequences without waiting on a 154-impl refactor.

So the sink has to be reachable from a free function called by a body that was not given one.

### Decision

`main` resolves the sink and publishes it into a `OnceLock` (`output::sink::install`); renderers read `output::sink::active()`.

- Before `install` runs, `active()` answers with a **piped** sink: not a terminal, width 0, no color, plain mode. The default is chosen so the failure mode of forgetting to install is unstyled untruncated output, never escape sequences written into a file.
- A duplicate `install` is logged at `debug` and ignored, not a panic. The sink is read-mostly configuration; aborting a user's command over it, or changing rendering halfway through one, are both worse than the second call being inert.
- `main` also calls `sink.apply_color_policy()`, which overrides the `colored` crate's own process-global env detection. `comfy_table` has no equivalent global, so `output::table` passes the same answer per render via `enforce_styling` / `force_no_tty`.
- Tests never call `OutputSink::from_process`. They build a sink with `OutputSink::resolve` and pass its answers to `Table::render(width, styled)` directly. The one test that flips the `colored` global serializes on a mutex and restores detection on drop.

Rejected alternative: **thread the sink through `Execute::execute`.** This is the correct end state and is what [Terminal Output Is a Rendering of a Structured Payload](#terminal-output-is-a-rendering-of-a-structured-payload) step 3 does. Rejected *for now* because it is the same 154-impl signature change that step 3 already owns; doing it here would either duplicate that churn or merge two independently reviewable changes, and would delay the `NO_COLOR` fix behind it. When step 3 lands, `active()` becomes removable in favor of the threaded value, and this ADR should be superseded rather than extended.

Rejected alternative: **have each renderer call `OutputSink::from_process()` itself.** Cheap and needs no global, but it re-derives terminal state per render — exactly the drift [Terminal Output Is a Rendering of a Structured Payload](#terminal-output-is-a-rendering-of-a-structured-payload) exists to remove — and would re-issue a `TIOCGWINSZ` ioctl per table. It also defeats `scripts/check-terminal-state-guard.sh`, whose whole premise is that one file queries the environment.

Rejected alternative: **a thread-local rather than a `OnceLock`.** Correct for concurrent in-process consumers, but `orbit-cli` renders from one thread and a thread-local would silently answer "piped" on any worker thread that rendered, which is a harder bug to see than the limitation below.

### Consequences

- `Table::print`, `output::color`, and `command/log/tail.rs` all read one answer, so the two styling backends can no longer disagree about `NO_COLOR`. That is the property [ORB-10570] exists to establish, and it is testable without a running `main`.
- The guard script's allowlist shrinks to the sink and its tests; `command/log/tail.rs` is no longer a grandfathered exception.
- Cost: a renderer's behavior depends on whether `main` ran. A unit test gets the piped default, which is the safe answer but not the interactive one, so a test that means to exercise the terminal path must build a sink explicitly and pass it — `Table::render(width, styled)` exists for exactly that, and `Table::print()` is the only function that reads the global.
- Cost: two sinks cannot be active concurrently in one process. An embedded or in-process invocation that wanted to render for a different destination would have to wait for the threaded sink of [Terminal Output Is a Rendering of a Structured Payload](#terminal-output-is-a-rendering-of-a-structured-payload) step 3. No such consumer exists today.
- Cost: `apply_color_policy` mutates the `colored` crate's process-global override, so any test that asserts on styled output must serialize against it. One mutex in `output/tests/gating.rs` carries that today; a second such test suite would have to share it rather than add its own.

## Task References

- [T20260411-0335] — introduced the table arrangement [Borderless Tables With Truncate-to-Width Rows](#borderless-tables-with-truncate-to-width-rows) reverses.
- [T20260427-43] — extended the duplicated color vocabulary [One Semantic Color Vocabulary, Gated at the Sink](#one-semantic-color-vocabulary-gated-at-the-sink) consolidates.
- [ORB-10356] — made `OrbitError` `#[non_exhaustive]`, establishing the error payload shape [Terminal Output Is a Rendering of a Structured Payload](#terminal-output-is-a-rendering-of-a-structured-payload) generalizes.
- [ORB-10570] — wired the sink into color and width, the implementation [The Output Sink Is a Process Global, Not a Renderer Parameter](#the-output-sink-is-a-process-global-not-a-renderer-parameter) documents.
- [ORB-10585] — filed [The Output Sink Is a Process Global, Not a Renderer Parameter](#the-output-sink-is-a-process-global-not-a-renderer-parameter), which [ORB-10570] could not allocate.
- [ORB-10586] — converted every command body to return a payload and gave the renderer sole ownership of stdout ([Terminal Output Is a Rendering of a Structured Payload](#terminal-output-is-a-rendering-of-a-structured-payload) steps 2–4), superseding [The Output Sink Is a Process Global, Not a Renderer Parameter](#the-output-sink-is-a-process-global-not-a-renderer-parameter).

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

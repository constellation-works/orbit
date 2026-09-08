---
type: design
summary: "Spec: Output Modes and Sink Resolution"
last_validated: 2026-08-31
---

# Spec: Output Modes and Sink Resolution

Every `orbit` command produces a structured payload and hands it to a renderer. The renderer resolves one output mode — `auto`, `table`, `json`, or `ndjson` — from global flags, environment, and the properties of the sink, once per invocation. A command body never decides how it is displayed, never asks whether stdout is a terminal, and never writes to stdout directly.

## Why This Exists

Before the migration, structured output was a per-command opt-in on 86 of 150 argument structs, hand-written alongside the human rendering in the same function, and the two drifted. Nothing detected whether stdout was a terminal, so a pipe received box-drawing characters and ANSI escapes. Both followed from rendering decisions living in command bodies. Rationale in [Terminal Output Is a Rendering of a Structured Payload](../4_decisions.md#terminal-output-is-a-rendering-of-a-structured-payload).

## 1. The Sink

The sink is resolved once at startup and answers every environment question. Nothing downstream may re-derive these.

| Property | Source | Fallback |
|----------|--------|----------|
| `is_tty` | `IsTerminal` on stdout | `false` |
| `width` | `COLUMNS` env, else terminal query | `0` (means: do not truncate) |
| `color_allowed` | see [./color-and-styling.md](./color-and-styling.md) §2 | `false` |

**Invariant:** `is_tty == false` implies `width == 0` and `color_allowed == false`. A non-terminal sink is never width-adapted or styled, regardless of what `COLUMNS` says.

`width == 0` disables truncation rather than assuming 80. Assuming a width for a sink that has none produces silently truncated data in a file, which is worse than a long line.

## 2. Mode Resolution

Precedence, first match wins:

1. `--format <mode>` explicitly passed.
2. `--json` (per-command legacy alias) → `json`.
3. `ORBIT_FORMAT` environment variable.
4. `auto`.

`auto` resolves to `table` when `is_tty`, and to the **plain** form otherwise. Plain is `table` with the header suppressed, borders and ANSI absent, truncation disabled, and single-tab field separators — the form `cut -f` expects. Plain is a rendering of `table`, not a fourth mode a command can request.

`--format` is one global user-facing argument installed across the command tree so it is accepted at the root and after subcommands; command-local `--format` arguments keep their own meaning. The existing per-command `--json` booleans remain accepted and hidden from help; they are not removed [Terminal Output Is a Rendering of a Structured Payload](../4_decisions.md#terminal-output-is-a-rendering-of-a-structured-payload).

## 3. Mode Contracts

| Mode | Shape | Truncates | Color | Streams |
|------|-------|-----------|-------|---------|
| `table` | aligned columns, header | yes, to `width` | if allowed | no |
| plain | tab-separated, no header | no | no | no |
| `json` | one document | no | no | no |
| `ndjson` | one document per line | no | no | **yes** |

- `json` for a list command emits a single array; for a detail command, a single object. It is pretty-printed only when `is_tty`.
- `ndjson` emits one complete JSON value per line and flushes per record. It is the only mode that may produce output before the command has finished collecting results, and the only correct choice for a long or unbounded list.
- `table` and `json` for the same invocation describe the same records. The table may omit fields and reformat values; it may not contain a value absent from the payload, and it may not omit a record the payload includes.

## 4. Payload Rules

- A payload is serializable and self-describing. Field names are `snake_case` and stable — renaming one is a breaking change to the CLI's contract, on the same footing as renaming a flag.
- Absent values are `null`, not omitted, so a consumer indexing by key does not have to distinguish "missing" from "not applicable".
- Timestamps are RFC 3339 with an explicit offset. Durations are integers in a unit named by the field (`duration_ms`), never pre-formatted strings.
- Values are typed as they mean: a count is a number, not a string. Formatting a number for display is the renderer's job.
- Enum-like values (status, state, role) are the canonical lowercase token. Display casing is applied at render time.

## 5. Streams

- **stdout carries the payload and nothing else.** Progress, warnings, counts, empty-state prose, and diagnostics go to stderr, in every mode.
- **Errors go to stderr.** In `json`/`ndjson` modes an error is a single JSON object on stderr using the existing `error_payload` shape (`error`, `code`, and the optional `did_you_mean` / `artifact_origin` / task-bundle fields); in other modes it is a plain message. Implemented in [ORB-10570]; the payload previously went to stdout, which is a **breaking change** for a script that parsed it there.
- **Exit codes are load-bearing.** `0` success, `1` command failure, `2` usage error. A command that printed an error object must not exit `0`.
- **A broken pipe is not an error.** `EPIPE` on stdout exits `0` silently — `orbit task list | head` must not print a panic.

## 6. Progress

- Spinners, progress bars, and status tickers are emitted only when `is_tty` and only to stderr.
- Progress output is erased before the process exits; a redirected stderr must not accumulate carriage-return frames.
- No progress in `json` or `ndjson`, even on a TTY. Those modes are chosen by consumers who are not watching.

## 7. Migration

1. ~~Introduce the sink and the global `--format`; leave every command body untouched.~~ Done [ORB-10569].
2. ~~Route the existing per-command `--json` branches through the resolver so precedence is centralized.~~ Done [ORB-10586]. `main` reads the invoked subcommand's `--json`/`--ops` boolean out of the parsed matches — the same walk `--format` uses — rather than from 86 argument structs, and passes it as `OutputSink::resolve`'s `legacy_json` rung.
3. ~~Convert command bodies to return payloads.~~ Done [ORB-10586] for the `Execute` signature; follow-up [ORB-11596] converted remaining record-output bodies that still forked on a command-local `--json` and wrote with `println!` / `CommandOutput::Silent`, so `--format json|ndjson` now projects those records through `output::render::emit`. Streams (`orbit log tail`) keep writing as they arrive and select JSONL vs the four-column view from `sink.mode()`, not from the local flag. [ORB-11622] converted `orbit workspace init` onto the same payload path. One remaining bypass has a separate owner and is not an omission: `orbit doctor --fix-*` repair counts ([ORB-11597]). Human-only confirmations that never had a structured document (for example `workspace remove`) still return `Silent`.
4. ~~Once a command returns a payload, delete its inline table construction and let the renderer own it.~~ Done for every list and detail command [ORB-10586]. A command builds a `Table` and hands it back inside the payload; `Table::print` is gone, and `Table::emit` is called only by the renderer.
5. ~~Move error output to stderr and audit exit codes.~~ Done [ORB-10570].

Steps 1 and 5 landed out of order deliberately: gating color and width at the sink (step 1's payoff) and moving errors off stdout (step 5) are both independent of the payload conversion, and holding them behind a 154-impl signature change would have left `NO_COLOR` broken for the duration.

Step 5 is the only user-visible break for existing scripts (an error object moves from stdout to stderr). It is recorded against [ORB-10570] for the release drafter; per `RELEASING.md` step 2, `CHANGELOG.md` is compiled at release time rather than accumulated per-PR, so the entry is written there, not here. Do not ship it quietly.

Steps 2–4 shipped together in [ORB-10586] and change three things for existing callers, none of them the bytes of a successful `--json` invocation (verified by diffing the pre-change binary against the new one across 20 commands):

- The **default piped form** of a list command is now plain — no header, tab-separated — where it used to be the header-bearing table. That is §2's contract finally taking effect; `--format table` asks for the old shape from a pipe.
- An explicit **`--format` now outranks `--json`** (rungs 1 and 2). While `--format` was inert, `orbit task list --json --format table` emitted JSON; it emits a table now.
- A **failing `--json` command reports its error as JSON on stderr**, because `--json` resolves the mode and §5 makes json-mode errors machine-readable. stdout is unaffected and still carries nothing on failure.

One deliberate deviation from §3, recorded here because it is a deviation: `--json` pretty-prints in every sink, while `--format json` pretty-prints only for a terminal. Every branch `--json` replaced called `print_pretty` unconditionally, and byte-identity for those invocations is an [Terminal Output Is a Rendering of a Structured Payload](../4_decisions.md#terminal-output-is-a-rendering-of-a-structured-payload) requirement, so the legacy rung keeps the bytes it had.

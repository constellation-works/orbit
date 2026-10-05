---
type: design
summary: "Glossary: Terminal Interface"
last_validated: 2026-09-20
---

# Glossary: Terminal Interface

Vocabulary specific to how `orbit` renders terminal output. Standard terminal and Unix terms (ANSI, TTY, stdout, pipe, `NO_COLOR`) are excluded — they carry their ordinary meanings. Included here are terms this feature gives a narrower meaning than usual, or coins outright. Web dashboard vocabulary is in [user-interface](../../user-interface/1_overview.md) and does not apply.

| Term | Meaning |
|------|---------|
| **`auto`** | The default output mode. Resolves to `table` on a TTY sink and to *plain* otherwise. Not itself a rendering — see [2_design.md §6](../2_design.md#6-per-command-structured-output) and [specs/output-modes.md §2](../specs/output-modes.md). |
| **Fixed column** | A column that never shrinks under width pressure — IDs, statuses, timestamps, durations. Contrast *flexible column*. [specs/table-rendering.md §2](../specs/table-rendering.md). |
| **Flexible column** | A column that may shrink to a floor of 8 display columns, then be dropped, when the result set exceeds the sink width. [specs/table-rendering.md §2](../specs/table-rendering.md). |
| **Mode** | One of `auto`, `table`, `json`, `ndjson`. Resolved once per invocation from flags, environment, and sink; never chosen by a command body. [specs/output-modes.md §2](../specs/output-modes.md). |
| **Payload** | The structured record a command produces, from which every rendering derives. The CLI's actual output contract; field names are as stable as flag names. [specs/output-modes.md §4](../specs/output-modes.md). |
| **Plain** | The piped form of `table`: no header, no borders, no ANSI, no truncation, tab-separated. A rendering of `table`, not a mode a caller can request. [specs/output-modes.md §2](../specs/output-modes.md). |
| **Renderer** | The layer that projects a payload into bytes for a mode. The only code aware of width, TTY state, or escape sequences. [1_overview.md §2](../1_overview.md). |
| **Role** | A semantic color token — `ok`, `warn`, `error`, `active`, `muted`, `neutral`. Attached to a value's meaning, mapped to ANSI only by the renderer, and never present in a payload. [specs/color-and-styling.md §1](../specs/color-and-styling.md). |
| **Sink** | The resolved stdout target together with its capabilities (`is_tty`, `width`, `color_allowed`). The single place every environment question is answered. [specs/output-modes.md §1](../specs/output-modes.md). |
| **Truncation indicator** | The single `…` marking a value cut to fit its column. Its presence is the promise that a detail command or `--format json` carries the value in full; a cut without one is silent loss. [specs/table-rendering.md §4](../specs/table-rendering.md). |
| **Uniform-value suppression** | Omitting, in `auto` mode only, a column whose value is identical across every row of the current result set. Computed per invocation, so it varies with filters. [specs/table-rendering.md §5](../specs/table-rendering.md). |

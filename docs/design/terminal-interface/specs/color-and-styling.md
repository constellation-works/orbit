---
type: design
summary: "Spec: Color and Styling"
last_validated: 2026-09-24
---

# Spec: Color and Styling

Color in `orbit` output is a semantic role attached to a value's meaning, mapped to ANSI by the renderer and by nothing else. Stripping every escape sequence from any `orbit` output loses emphasis and loses no information. A command never names a color, never calls a styling crate, and never asks whether color is permitted.

## Why This Exists

`crates/orbit-cli/src/output/color.rs` defines the same status vocabulary twice — once returning `comfy_table::Cell`, once returning `colored`-styled `String` — because the styling backend is chosen by the call site's rendering shape rather than by what the value means. Every status change costs two edits, the copies have already drifted, and the two backends disagree about whether `NO_COLOR` applies. Rationale in [One Semantic Color Vocabulary, Gated at the Sink](../4_decisions.md#one-semantic-color-vocabulary-gated-at-the-sink).

## 1. The Role Vocabulary

Six roles, closed set. Adding a seventh is a design change, not a call-site decision.

| Role | Meaning | Typical rendering |
|------|---------|-------------------|
| `ok` | terminal, succeeded | green |
| `warn` | needs attention, not failed | yellow |
| `error` | failed, denied, blocked | red |
| `active` | in flight right now | cyan |
| `muted` | de-emphasized, archived, secondary | dim |
| `neutral` | no signal | default |

Mapping from domain values to roles is defined once, in one table, covering task status, run state, job state, priority, task type, doctor status, and audit status together. The current eight per-domain functions (`status_color`/`_cell`, `priority_color`/`_cell`, `job_state_color`/`_cell`, `doctor_status_color_cell`, `task_type_color_cell`) collapse into it.

**Unmapped values are `neutral`, never an error.** A new status introduced elsewhere in the codebase must render plainly, not panic and not fall through to an arbitrary color.

## 2. When Color Is Emitted

Resolved once, at the sink:

1. A non-terminal sink → off.
2. `TERM=dumb` → off.
3. `NO_COLOR` set to any non-empty value → off.
4. `CLICOLOR_FORCE` set to any non-empty value → on.
5. Otherwise: on for a terminal.

There are no command-line color override flags; the sink owns this environment-based policy.

**Invariant:** exactly one place in the crate reads these — `output/sink.rs`, enforced by `scripts/check-terminal-state-guard.sh`. A call site that consults them itself is a defect. Because the two styling crates each ship their own probe, "one place" also means the sink must *override* them rather than agree with them: `OutputSink::apply_color_policy` sets `colored`'s global override, and `output::table` passes `enforce_styling`/`force_no_tty` per render. Neither backend is left to ask.

## 3. Rules

- **Color is never the sole carrier of meaning.** A status cell prints its word; the word is sufficient with color stripped. This is what makes the piped and `NO_COLOR` paths correct rather than degraded.
- **Roles apply to values, not rows.** A failed run's `STATE` cell is red; its row is not. A row-level wash destroys the scan pattern the columns exist to create.
- **Bold is structure, not severity.** Headers and the primary identifier column may be bold. A value is never bolded to mean "worse" — that is what `error` is for.
- **Dim is the only other attribute.** No italic (inconsistently supported), no underline (conflicts with terminal link handling), no blink, no reverse video, no background colors.
- **16-color ANSI only.** No 256-color or truecolor. Orbit output has to be legible against terminal themes it does not control, and the basic palette is the only one users have reliably themed.
- **Never restyle a value the user is filtering on.** If `--status blocked` was passed, the status column is informationally uniform; leave it neutral rather than painting every row red.

## 4. Interaction With Modes

- `json`, `ndjson`, and the plain piped form carry **no** escape sequences. Color policy applies only to human-rendered table and line output.
- Roles are a rendering concern and never appear in a payload. A payload carries `status: "blocked"`, not `status_role: "error"`. A consumer that wants severity derives it from the value, using the same public mapping.

## 5. Accessibility

- Red/green must never be the only distinction between two states a reader has to tell apart. §3's first rule already implies this, since the words differ — but it is worth checking per column rather than assuming, especially for columns that render a glyph or a bare count.
- The palette needs a real contrast check against common dark and light terminal themes. It has never had one; see [../2_design.md §9](../2_design.md#9-concerns--honest-limitations).
- `muted` (dim) is the highest-risk role: on some themes dim text falls below usable contrast. Never use `muted` for a value the reader must be able to read — only for values they may skip.

## 6. Migration

1. ~~Define the role enum and the single domain-value mapping table.~~ Done.
2. ~~Reimplement the eight existing `*_color*` functions as thin wrappers over it, so no call site changes yet and the drift is fixed immediately.~~ Done — this alone resolved the `backlog` inconsistency between `status_color` and `status_color_cell`.
3. ~~Move emission gating into the sink; delete the local `is_terminal` check in `command/log/tail.rs`.~~ Done [ORB-10570].
4. ~~Replace the wrappers with role-tagged values at each call site and delete the wrappers.~~ Done [ORB-10570]. A call site now passes either a `Role` it names outright or the `Domain` the value came from; `cell(value, tag)` and `text(value, tag)` are the only two renderings.

**Remaining gap:** §3's "16-color ANSI only" is not met on the table path. `comfy_table` renders a role's color through `crossterm` as a 256-color code (`\e[38;5;10m` for green) while `colored` renders the same role as `\e[32m`. Both honor the sink's decision about *whether* to emit; they disagree about the shade. Closing it means either mapping roles to raw SGR codes and bypassing `comfy_table::Color`, or accepting the 256-color spelling and amending §3.

**Remaining gap:** §3's "never restyle a value the user is filtering on" is unimplemented. `Column::filtered` keeps such a column on screen, but the cell is still painted, in every list command.

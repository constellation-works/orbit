---
type: design
summary: "Reference: Detail Commands Behind Truncatable List Columns"
last_updated: 2026-11-02
last_validated: 2026-11-02
---

# Reference: Detail Commands Behind Truncatable List Columns

[specs/table-rendering.md §4](../specs/table-rendering.md) makes truncation a promise: a list command that can cut column *C* must have a named command that prints *C* in full for one record. This table records, for every list view rendered through `crates/orbit-cli/src/output/table.rs`, which columns can be truncated and where the whole value lives (ORB-10567).

Only *flexible* columns are ever truncated — fixed columns render whole or are absent — so a view with no flexible column needs no detail counterpart. `--format json` / `--json` and plain output carry full values everywhere; human detail selectors below recover one record without terminal-width truncation.

`orbit task add` requires `--title` and `--complexity` (low, medium, hard or
xhard); its `--description` remains optional. The tool-call surface has its own
requirements: `orbit tool show <name>` marks fields required by schema branches
as `conditional` and prints the object-level input constraints as JSON Schema.
For `orbit.task.add`, ordinary creation requires `description` and `complexity`;
with `request_id`, `workspace`, `description` and `acceptance_criteria` are required
and `complexity` is not accepted. The same rendering applies to other tools with
conditional schemas, including `orbit.task.update` and `orbit.pipeline.invoke`.
`--parameter <name>` includes the same constraints. JSON inspection includes the
full declared `input_schema` and each parameter's `requirement` (`required`,
`conditional` or `optional`); the existing `required` boolean continues to describe
unconditional requirements from the flat parameter metadata.

`orbit tool show <name>` also prints persisted metadata for disabled external tools listed by `orbit tool list --all`. Inspection preserves their disabled state; `orbit tool run` remains refused until the tool is enabled.

## Covered

| List view | Truncatable columns | Detail command |
|-----------|--------------------|----------------|
| `orbit tool list` | `REQUIRED INPUT`, `DESCRIPTION` | `orbit tool show <name>` |
| `orbit audit list` | `TOOL` | `orbit audit show <id>` |
| `orbit auto-task list` | `TITLE` | `orbit auto-task show <name>` |
| `orbit task list` | `TITLE` | `orbit task show <id>` |
| `orbit job list` | `TARGET_ID` | `orbit job show <job_id>` |
| `orbit run history` | `ERROR` (already capped at 60 characters), and with `--failures` the flexible `CAUSE` column | `orbit run show <run_id>`; `--failures` prints that command in `SHOW` for the root-cause run |
| `orbit run events` | `SUMMARY` | `orbit run trace <run_id>`, `orbit run logs <run_id>` |
| `orbit run show` (step summary) | `TARGET`, `ERROR MESSAGE` | `orbit run show <run_id> -s <step>` |
| `orbit routine list` | `SOURCE` | `orbit routine show <name>` |
| `orbit skill list` | `SUMMARY` | `orbit skill show <id>` |
| `orbit friction list` | `TAGS`, `TITLE` | `orbit friction show <id>` |
| `orbit search` | `ID/PATH`, `TITLE/SUMMARY` | per hit kind: `orbit task show`, `orbit friction show` |
| `orbit codebase analytics` | `PATH`, `PATHS`, `RANKINGS` | `orbit codebase show <path>` (rankings, counts and the tasks behind each); `--json` for coupled pairs and the regression cross-tabs |

## Diagnostic and nested detail recovery

| List view | Truncatable columns | Full single-record command |
|-----------|--------------------|----------------------------|
| `orbit doctor` | `DETAILS` | `orbit doctor --check <check>` |
| `orbit tool doctor` | `DETAILS` | `orbit tool doctor <name>` |
| `orbit skill doctor` | `DETAILS` | `orbit skill doctor <id>` |
| `orbit tool show <name>` (parameters) | `DESCRIPTION` | `orbit tool show <name> --parameter <parameter>` |

Each selector uses the exact name printed in the list (CHECK, TOOL, SKILL or
parameter NAME). It prints a labeled detail record with the full message or
description, including embedded newlines; terminal width never cuts it. An
unknown selector fails explicitly. JSON returns the selected record as an
object (skill detail groups all diagnostics for that skill in `findings`);
unselected lists retain their existing complete arrays and plain rows.
Workspace doctor detail includes remediation and retains the selected check's
failure exit status. `--check` cannot combine with `--problems` or repair flags.
The selector chooses output after diagnostics run; it does not promise to run
only that check or tool.

Workspace doctor's full non-ok findings section already landed [ORB-14829].
It remains available below the terminal table; `--check` also recovers ok/info
rows that intentionally have no findings section. This closes the remaining
per-record gaps [ORB-21575] without duplicating findings in list rows.

`orbit migrate` is a fixed-shape status readout: COMPONENT, CURRENT and
SUPPORTED are all fixed columns. Component labels (`workspace layout` and
`store schema`) therefore stay whole, even on a narrow terminal; overflow is
reported on stderr. No migration column is truncatable and no deeper command
is required. Plain and JSON retain the full report.

## Gaps

None in the views audited above [ORB-21575]. Adding a flexible column still
requires a documented single-record command under the accepted contract.

## Run history

`orbit run history` shows TASK (comma-separated task IDs) and DURATION
(human-readable recorded wall time). Runs without task bindings have an empty
TASK cell; missing durations have an empty DURATION cell. The ERROR cell is
capped at 60 characters. `--failures` replaces that table with one row per task
and root cause: TASK, the root-cause RUN_ID, STEP, CREW, CAUSE (the first line
of that run's error, uncapped, and the only flexible column), and SHOW
(`orbit run show` for the root-cause run). Error details remain in `orbit run
show`, which also displays each task ID and its title when the task still
exists.

Combine `--task <id>`, `--state failed,held`, and `--since 24h` to select a
recent delivery history. `--task` matches exact IDs in the submitted `task_ids`
array or the singular `task_id` input, the same bindings the TASK column shows.
`--state` accepts comma-separated run states; `--since` accepts a relative
duration or RFC 3339 timestamp and compares creation time. All predicates run
in the store before ordering and `--limit`, so unrelated newer runs cannot
hide matching history. Existing `--job` and `--no-reconcile` options still apply.

CLI run JSON and `orbit.run` action `list` expose `task_ids` using the same
nullable array as the dashboard: sorted, unique, nonempty IDs extracted from
submitted `task_ids`/`task_id`, or null for a task-free run. Listing identities
never reads task records; title lookup is reserved for detail views.

## Task References

- [ORB-10567] — Replace bordered wrapping tables with borderless one-line rows and column metadata.
- [ORB-14829] — Show full workspace-doctor findings and remediation below its terminal table.
- [ORB-21575] — Add full detail recovery for audited truncated views.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

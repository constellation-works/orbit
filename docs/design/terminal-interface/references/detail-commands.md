---
type: design
summary: "Reference: Detail Commands Behind Truncatable List Columns"
last_updated: 2026-10-03
last_validated: 2026-09-26
---

# Reference: Detail Commands Behind Truncatable List Columns

[specs/table-rendering.md §4](../specs/table-rendering.md) makes truncation a promise: a list command that can cut column *C* must have a named command that prints *C* in full for one record. This table records, for every list view rendered through `crates/orbit-cli/src/output/table.rs`, which columns can be truncated and where the whole value lives (ORB-10567).

Only *flexible* columns are ever truncated — fixed columns render whole or are absent — so a view with no flexible column needs no detail counterpart. `--format json` / `--json` carries full values everywhere and is the fallback where no detail command exists.

`orbit tool show <name>` also prints persisted metadata for disabled external tools listed by `orbit tool list --all`. Inspection preserves their disabled state; `orbit tool run` remains refused until the tool is enabled.

## Covered

| List view | Truncatable columns | Detail command |
|-----------|--------------------|----------------|
| `orbit tool list` | `REQUIRED INPUT`, `DESCRIPTION` | `orbit tool show <name>` |
| `orbit audit list` | `TOOL` | `orbit audit show <id>` |
| `orbit auto-task list` | `TITLE` | `orbit auto-task show <name>` |
| `orbit task list` | `TITLE` | `orbit task show <id>` |
| `orbit job list` | `TARGET_ID` | `orbit job show <job_id>` |
| `orbit run history` | — (fixed columns) | `orbit run show <run_id>` (task titles, full errors), `orbit run show <run_id> -s <step>` |
| `orbit run events` | `SUMMARY` | `orbit run trace <run_id>`, `orbit run logs <run_id>` |
| `orbit run show` (step summary) | `TARGET`, `ERROR MESSAGE` | `orbit run show <run_id> -s <step>` |
| `orbit routine list` | `SOURCE` | `orbit routine show <name>` |
| `orbit skill list` | `SUMMARY` | `orbit skill show <id>` |
| `orbit friction list` | `TAGS`, `TITLE` | `orbit friction show <id>` |
| `orbit search` | `ID`, `TITLE/SUMMARY` | per hit kind: `orbit task show`, `orbit friction show` |

## Gaps

These views can truncate a column and have no detail command. Listed rather than invented — closing them is separate work, and `--json` is the interim answer.

| List view | Truncatable columns | Note |
|-----------|--------------------|------|
| `orbit doctor` | `DETAILS` | Diagnostic output; the message is authored short. No per-check detail command. |
| `orbit tool doctor` | `DETAILS` | As above. |
| `orbit skill doctor` | `DETAILS` | As above. |
| `orbit tool show` (parameters) | `DESCRIPTION` | Already the detail view; the parameter description has no deeper surface than `--json`. |

`orbit migrate status` is absent from both tables: every column it renders is fixed or numeric.

## Run history

`orbit run history` shows TASK (comma-separated task IDs) and DURATION
(human-readable recorded wall time). Runs without task bindings have an empty
TASK cell; missing durations have an empty DURATION cell. Error details remain
in `orbit run show`, which also displays each task ID and its title when the
task still exists.

Combine `--task <id>`, `--state failed,held`, and `--since 24h` to select a
recent delivery history. `--task` matches exact IDs in the submitted `task_ids`
array. `--state` accepts comma-separated run states; `--since` accepts a relative
duration or RFC 3339 timestamp and compares creation time. All predicates run
in the store before ordering and `--limit`, so unrelated newer runs cannot
hide matching history. Existing `--job` and `--no-reconcile` options still apply.

CLI run JSON and `orbit.workflow.run.list` expose `task_ids` using the same
nullable array as the dashboard: sorted, unique, nonempty IDs extracted from
submitted `task_ids`/`task_id`, or null for a task-free run. Listing identities
never reads task records; title lookup is reserved for detail views.

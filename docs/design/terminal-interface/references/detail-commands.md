---
type: design
summary: "Reference: Detail Commands Behind Truncatable List Columns"
last_updated: 2026-09-26
last_validated: 2026-09-26
---

# Reference: Detail Commands Behind Truncatable List Columns

[specs/table-rendering.md §4](../specs/table-rendering.md) makes truncation a promise: a list command that can cut column *C* must have a named command that prints *C* in full for one record. This table records, for every list view rendered through `crates/orbit-cli/src/output/table.rs`, which columns can be truncated and where the whole value lives (ORB-10567).

Only *flexible* columns are ever truncated — fixed columns render whole or are absent — so a view with no flexible column needs no detail counterpart. `--format json` / `--json` carries full values everywhere and is the fallback where no detail command exists.

## Covered

| List view | Truncatable columns | Detail command |
|-----------|--------------------|----------------|
| `orbit tool list` | `REQUIRED INPUT`, `DESCRIPTION` | `orbit tool show <name>` |
| `orbit audit list` | `TOOL` | `orbit audit show <id>` |
| `orbit auto-task list` | `TITLE` | `orbit auto-task show <name>` |
| `orbit task list` | `TITLE` | `orbit task show <id>` |
| `orbit job list` | `TARGET_ID` | `orbit job show <job_id>` |
| `orbit run history` | `ERROR_MESSAGE` | `orbit run show <run_id> -s <step>` |
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

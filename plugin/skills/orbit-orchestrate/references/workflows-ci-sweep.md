# Pilot and file CI sweep repairs

## Sweep filing and pilot admission

CI-sweep filing is deliberately non-executable: `file_ci_failure_tasks` always
creates `proposed` tasks. The CI job invokes `task_pilot_pipeline` for each new
task and retries matching tasks that a prior pilot left proposed, carrying
explicit promotion authority into its deterministic apply boundary. Invalid or
empty selectors, pilot failure, duplicates, already-landed
work, conflicts, holding warnings and a `no-auto-approve` tag leave that task
proposed without blocking other pilot children.

A `blocked_by` finding, or a
holding utility or surface finding, holds unless it names prerequisites and every
one is already done — in this workspace or in another registered workspace,
resolved through its registered owner — or is enforced by a stored dependency edge.
Free text, missing tasks and unfinished prerequisites still hold, wherever
they are registered.

Admission decides from the edges in the atomic pilot
write and completion statuses read under the dependency locks; replay uses
only already stored edges. A drain rechecks named prerequisites under their
locks before approval. `adr_conflicts`, `contract_conflicts` and deterministic
`validation_tool_warnings` entries always hold; `contract_conflicts` entries carry
no `holds_promotion` field and have no opt-out.

A criterion that contradicts the
pinned code or contract goes in `contract_conflicts`, never in a surface or utility
finding. Surface and utility findings use `{text, holds_promotion}`: true holds for
wrong behavior or an unreachable validation requirement; false preserves an
advisory observation without holding promotion or showing it as held. Legacy
plain-string surface and utility warnings still hold unless they name only
recorded dependencies or done tasks.

Advisory findings remain
in the assessment artifact named by the one-line receipt in `orbit task show` and the worker's brief; fetch it with `orbit.task.artifact` action `get`. An
`orbit run auto --approve-proposed` drain hands the same apply boundary a drain-scoped authority instead, verified
against the drain that dispatched the pilot. A standalone task-pilot run has no
promotion authority.

The
source run/job/SHA/step remains in the task description, while parent and child
run state retain the pilot run ID, result, and admission decision. Filing
clusters failures by a normalized error signature that prefers a concrete test
or panic identity over ANSI styling, generic runner/cargo/nextest trailers, and
assertion payload help text; the raw excerpt stays in the description.

Runner `Run` groups retain printed-command provenance through their closing
`##[endgroup]`: command headers and bodies are reproduction context, never
emitted failure anchors. An unclosed group or a prefix ending in an orphan
closing delimiter cannot establish a source cause. Genuine emitted annotations,
compiler diagnostics and test/panic identities keep their precedence after the
command display closes.

Repair excerpts stay bounded around that same emitted
diagnostic, including when later cleanup would fill a tail-only excerpt.
Landing refuses attribution from incomplete sources or truncated displays
without a complete failing command; this does not grant any new repair or
merge authority.

The sweep routes each repair to a host that can reproduce it. Collection reads
each failed job's runner labels from the GitHub jobs API; when a snapshot has
none, filing falls back to the workflow's literal `runs-on` in the checkout.
`macos-*` tags the task `os:macos` and `ubuntu-*`/`linux` tags it `os:linux`;
a Windows or unrecognised runner, or a cluster mixing one in, adds no `os:`
tag, because no Windows host exists and an unclaimable task would be worse
than one any host may take.

The `Runner OS:` description line and the filing's
`runner_os` entries record the evidence. A Linux owner then leaves an
`os:macos` repair in its backlog for a macOS pull-drain follower instead of
blocking on it. To reroute any task, retag it with `orbit.task.update` (see
[task authoring](../../orbit/references/task-authoring-create.md#host-os-routing)).

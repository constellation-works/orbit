# Task fields: required and granted tools

Contents:

- [Behavior-affecting optional fields: tools and grants](#behavior-affecting-optional-fields-tools-and-grants)
- [Operator runbook: granting tools](#operator-runbook-granting-tools)

## Behavior-affecting optional fields: tools and grants

- `required_tools: ["<exact.canonical.tool>", ...]` — tools the task requires.
  Allowlist activities add them to their baseline; deny-list activities refuse
  a requirement covered by `tool_disallow_list`, which it never overrides.
  Filing refuses a covered name no grant could lift — a mutating tool, an
  always-operator-only tool, or a name a wildcard entry still covers — and
  names the tool, the activity and the reason.
  - Have an operator perform that
    step, or file it as a separate operator-owned task. A read/repeat-safe tool
    denied by an exact entry can still be required.
    Use only exact, active, agent-facing registered
    names; wildcards and prefixes are rejected at dispatch. The list is normalized,
    sorted, and deduplicated at creation.
  - It is immutable afterward, and every
    existing-task update surface rejects `required_tools` — so declare every tool
    your acceptance criteria name **at creation**, because setting it later
    cannot widen an already-dispatched activity. → [Validation your lane can
    actually run](task-fields-validation.md#validation-your-lane-can-actually-run)
  - Inclusion grants only activity allowlist membership: caller role, host
    capability, tool policy, filesystem/subprocess policy, and external
    authentication can still deny execution.
  - A task that names exactly
    `github.auth.status`, `github.run.list`, `github.run.view`,
    `github.run.logs`, and `github.pr.list` is the worked example:
    an allowlist activity stays unchanged and
    `effective_tools = activity baseline ∪ those five`.
  - The shipped deny-list
    `agent_implement` already includes those GitHub reads. Reaching
    `github.auth.status` can still yield a structured `available: false` or
    `authenticated: false` capability-unavailable result when the lane has no
    GitHub client or credentials; that is not a clean CI pass.
- `granted_tools: ["<exact.canonical.tool>", ...]` — tools an **operator**
  grants the implementing activity despite its deny list. Separate from
  `required_tools`: only an operator session may set or change it, on
  `orbit.task.add` or `orbit.task.update`, and any other caller that supplies
  the field (even `[]`) is refused naming the missing operator authority,
  never silently stripped. Names are checked like `required_tools`.
  - An update
    replaces the list (`[]`, or CLI `--granted-tools ""`, clears it) and is
    accepted only while the task is `proposed`, `backlog` or `someday` with no
    live run, footprint-protecting execution claim or worktree. A preserved
    worktree keeps the grant frozen after its run stops or claim is released;
    requeue and remove any preserved worktree before changing the grant for a
    new attempt.
  - Every change records a `granted_tools_changed` history entry
    naming the writer, both tool sets and the run/claim/worktree state. Only
    `agent_implement` reads a grant;
    review, recovery, pilot and every other activity ignore it. A grant lifts
    the deny list only for a tool classified read/repeat-safe.
  - Every tool
    `agent_implement` denies today is classified mutating, so such a grant
    stays denied and is never called live, and also listing it in
    `required_tools` is refused at filing and still fails implementer admission.
  - A lifted tool still
    passes every other check: capability and field-level operator gates,
    filesystem and process policy, and claimed-mode denials.
    An auto-task definition can carry the same list
    (`orbit auto-task update <name> --granted-tools …`, operator only), which
    is copied to every task it mints.
- **Mutation proposals** carry a mutating grant's call instead. Attach
  `mutation-proposals/<name>.json` with `orbit.task.artifact` action `put`, content
  `{"tool": "<granted tool>", "input": {…exact tool input…}, "rationale":
  "<why>"}`. It is refused unless the task grants the tool, the tool is a
  grantable mutation, and an agent session could make that exact call (a
  grant never satisfies a field-level gate such as `expected_enabled`).
  - Orbit stores it with the task id and an `idempotency_key` derived from the
    task, tool and input; nothing changes live. The review gate's manifest
    lists every unapplied proposal for the reviewer, and when that review
    passes the pipeline applies each one once, with the reviewed input, and
    records a `mutation-receipts/<key>.json` receipt (`applied` or `failed`).
    Receipts are Orbit's alone.
  - A re-emitted identical proposal never applies
    twice; a changed one is a new key that waits for the next review. A failed
    or unreviewed proposal fails the review step with a task comment, a run
    that reviews nothing refuses the task, and a task cannot become `done`
    while a proposal is unapplied. A change the agent needs during its run
    cannot be proposed: it is an operator action or a prerequisite task.

## Operator runbook: granting tools

Use an operator session on the task's owner host. Inspect the task and the
exact tool schema first (`orbit task show <id>`, `orbit tool show <tool>`).
Decide whether the work needs a reviewed mutation or a prerequisite operator
action: all currently denied implementer tools are mutating, so a grant does
not enable their live execution.

Do not also put a denied mutation in
`required_tools`; filing refuses it, and a task that already carries it
still refuses implementer admission. Have an operator perform that step,
or file it as a separate operator-owned task.

1. Before dispatch, set the complete grant list with
   `orbit task update <id> --granted-tools orbit.auto_task:update`.
   Creation also accepts `orbit task add --title "<title>" --complexity low --granted-tools
   orbit.auto_task:update`. These are operator commands; an agent must not
   claim operator authority to run them. A replacement is the whole list,
   not an append; `--granted-tools ""` revokes it before the next attempt.
2. Confirm `Granted Tools` beside `Required Tools` in task show/list and
   inspect `orbit task show <id> --fields history`. The
   `granted_tools_changed` entry records the operator identity, timestamp,
   before/after lists and dispatch state. A grant pins execution to the owner
   host. If required labels exclude the owner, resolve that intersection;
   a follower carrying the labels cannot take over the grant.
3. Have the implementer attach the proposal described above, then inspect it
   with `orbit task show <id> --fields artifacts` and
   `orbit tool run orbit.task.artifact --input
   '{"action":"get","id":"<id>","path":"mutation-proposals/<name>.json"}'`.
   Review the exact input with the diff. A proposal-only delivery without a
   reviewed diff is refused. Approval applies the reviewed proposal before
   push/landing; verify its system-written receipt afterward.
4. Diagnose refusals by their named gate. An exact disallow or wildcard
   refusal is a deny-policy gate; `orbit.routine.control` or another missing
   capability is a capability gate; an owner-host/label wait is placement.
   None is resolved by adding `required_tools`. To change a frozen grant,
   stop/requeue the attempt and remove its preserved worktree first.

`orbit audit list --kind operation --json` exposes authorization decisions;
the `command = "authorization"` rows include grant authorizations (operation
`orbit.task.grant_tools`) and activity-policy/capability denials. Match them
with task history to distinguish an authorized attempt from a committed grant
change. `orbit audit stats --tool orbit.auto_task:update --json` gives the
existing denial-by-operation breakdown; it counts a decision once even when
an entry-point audit row repeats it.

A `failed` receipt is evidence of a failed application. A `pending` receipt
means the result is unknown: do not retry it or edit/forge a receipt. Inspect
the receipt and the actual owner state, retain evidence, and resolve the
outcome as separate operator work. There is no automatic pending-receipt
reconciliation command. A changed proposal creates a new key and requires a
new review; it must not be used to repeat an unknown outcome.

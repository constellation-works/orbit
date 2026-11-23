# Task fields: validation your lane can run

Contents:

- [Validation your lane can actually run](#validation-your-lane-can-actually-run)
  - [Native host evidence](#native-host-evidence)
  - [User-facing UI](#user-facing-ui)

## Validation your lane can actually run

A criterion that asks the implementer to call a tool is a promise about the lane that will run it, and
`required_tools` records that requirement — which is why it
has to be right at creation.

Task-pilot preparation reads each criterion
against the registered tool surface, the canonical MCP tool list, the
implementation activity's allowlist, and the governed-operation registry, and
reports one `validation_tool_warnings` finding per contradiction before the
task is admitted, including a stored `required_tools` entry the implementing
activity disallows and no grant could lift. That finding holds promotion of
a proposed task.

Preparation itself still completes. Transport, allowlist,
credential, and utility findings do not withhold an already-approved backlog
task. A current
operator-reserved validation requirement holds local workflow admission and
owner pull claims until an operator handles it, and a criterion that needs
native evidence from an OS the task's `os:` tags do not name holds a host of
another OS (see [Native host evidence](#native-host-evidence)).

- **Transport.** An MCP session reaches only MCP-advertised tools. `proc.spawn`
  is registered CLI-only, so a criterion that requires it over MCP can never
  pass — drive it through `orbit tool run proc.spawn` instead. Never ask for
  the MCP surface to be widened to match a criterion's wording.
  `proc.spawn` clamps a larger `timeout_ms` to its ceiling: 60 seconds (60000
  ms) outside a managed activity; inside one, the activity's remaining
  wall-clock budget, at most 45 minutes.
  - Its result reports the applied `timeout_ms`,
    `timeout_ceiling_ms`, `timeout_ceiling_source`, `timeout_clamped` with a
    `timeout_notice` when clamped, and the explicit `requested_timeout_ms` when
    supplied. A timed-out result includes a `hint` with `transport: native_shell`
    and a message directing long validation to that transport.
  - Run build, test,
    cargo and make validation that can take minutes in the provider's native
    shell session (Codex: `exec_command`/`write_stdin`) or another long-running
    transport the lane provides. A `proc.spawn` timeout on cargo/make is never a
    validation blocker; rerun there and await completion, including shared
    build-budget admission. Preserve sandboxing and build-budget admission.
- **Activity tools.** For an allowlist activity, a tool outside its baseline
  has to be in `required_tools`. A deny-list activity exposes registered
  agent-facing tools except its disallowed names; `required_tools` cannot
  override a denial. Filing refuses an unliftable covered requirement, and a
  stored one still refuses dispatch. Task-pilot reports that stored
  requirement as a blocking `validation_tool_warnings` finding before
  dispatch.
  - A read/repeat-safe exact denial can still be required. Screenshot and
    task-artifact attachment is part of the implement lane. Owner mode attaches
    with `orbit.task.artifact` action `put`. Claimed mode attaches declared files through
    the `task_artifacts` handoff even without that tool. A criterion that only
    requires those screenshots is feasible; task-pilot does not warn that
    artifact attachment is unavailable because `required_tools` is empty.
  - A
    criterion that asks the implementer to call a tool the lane does not grant,
    and that `required_tools` does not declare, is reported. A criterion that
    names an ungranted tool only as the subject of the change — its behaviour,
    schema or documentation, proven by the task's own tests or doc check rather
    than by calling it — is not reported by the deterministic allowlist check.
- **Operator capability.** Governed operations — workflow run observation and
  resume, `orbit.command.exec`,
  and the other destructive surfaces — are reserved for an operator.
  `required_tools` grants allowlist membership, not capability, and an
  implementing agent must never set an authority environment variable to get
  past that gate.
  - Write the criterion as an explicit operator handoff, or scope
    it to the registered dispatch path an agent can reach.
  - A criterion that
    names an operator-only tool only as the **subject** of the change (its code,
    description, schema, docs or tests: "`orbit.task.recover` action `requeue` refuses a dirty
    handoff, shown by a runtime test") does not hold: the pilot classifies each
    detected mention as `subject` or `invoke` with a one-line reason in its
    assessment's `operator_tool_classifications`.
  - Only `invoke` (meeting the
    criterion means running the tool) becomes an operator validation hold and
    keeps this finding; a mention the pilot leaves unclassified, or classifies
    malformed, holds as `invoke`.
- **External credentials.** Calling a `github.*` tool against live GitHub
  depends on authentication the lane may not hold, so state that precondition
  or hand the check to an operator instead of treating a capability-unavailable
  result as a pass.
  - A criterion that names `github.*` only as the subject
    under test, or whose sentence states in the same clause as the call a
    credential-free run (such as a fake `gh` or no live API), is not flagged.
    Credential, offline or authentication wording in another clause or sentence
    does not clear a live call.

A criterion that *expects* a refusal is a correct negative test, and a tool
name inside a quoted example is a copied observation, not a requirement.
Neither is reported, and neither grants anything.

The pilot commits a typed `operator_validation_hold` in its assessment audit and
an `operator_validation_held` history event, naming each one-based criterion and
canonical tool. This hold also applies when a human approved the task before the
pilot ran.

It is scoped to the assessed task material: a changed criterion makes
the old assessment stale, while priority changes and discussion leave it current.
An operator can re-scope the criterion or add a human task comment with one of
these first lines and non-empty evidence on subsequent lines:

- `task-pilot-admission: evaluated` — describe the completed evaluation and
  reference a non-empty evaluation artifact attached to the task.
- `task-pilot-admission: clear` — explain why the requirement is satisfied.
- `task-pilot-admission: approve-anyway` — record the deliberate override and why
  the managed task can proceed.

These human updates record `operator_validation_resolved`. Attaching an artifact
alone or copying the header into an agent comment does not resolve the hold.
A new pilot assessment supersedes an earlier decision. No resolution grants a
tool or changes governed-operation authority.

Older pilot receipts without the typed material snapshot are interpreted only
while no document edit follows the assessment. Their governed-operation warnings
are checked against the current registry and positive criterion mentions; all
other warning kinds stay advisory.

### Native host evidence

When a criterion needs execution or evidence that only a native host of one OS
can produce (a real macOS `sandbox-exec` launch, a Linux Bubblewrap result), the
pilot records a typed `required_os` finding naming the one-based criterion and
the OS, and commits it as `native_os_hold` in its assessment audit.

Apply
rejects an entry whose criterion text names no OS or native facility (mac,
macos, darwin, seatbelt, sandbox-exec, linux, bwrap, bubblewrap, windows,
launchd, systemd, keychain, cgroup), unless its evidence cites one
`#[cfg(target_os = "<os>")]` gate at a repository-relative `path:line` that
apply confirms in the pinned source, on an item the criterion names, and the
entry's `os` is that OS.

A portable criterion, a gate on an item the
criterion does not name, an `os` that differs from the gate, a path or line
the pin does not confirm, and a `cfg(unix)` or `cfg(not(windows))` gate that
does not determine one OS are still rejected, and the rejection tells the
pilot to leave `required_os` empty.

One further exception is a
repository-required validation: when a tag the task carries triggers a
validation the workspace guide (`AGENTS.md`/`CLAUDE.md`) requires and states
only Linux can produce, the pilot may cite
both guide lines at their `path:line`
(the tag-to-CodeQL requirement and the only-Linux statement) instead of a
cfg gate, and apply confirms both in the pinned source before admitting
`os: linux`.

In this repository that validation is a `security`-tagged task's
local CodeQL run, where the guide line names the trigger tag in backticks after
`tagged` and other words on that line trigger nothing. The pilot makes no such finding when the task or a workspace
rule explicitly routes that check to owner/external evidence after handoff,
and a platform mention, cross-compilation target or mocked check is never
such a finding.

When every
finding names one OS and the task carries no `os:` tag, the same atomic write
adds that tag (`os:linux`), so admission routes the task by its tags and a host
of another OS waits as `host_os_mismatch` (see [Executor definitions](../../orbit-setup/references/configuration-execution.md#executor-definitions)). The pilot never removes or replaces
an `os:` tag already on the task.

While those tags do not name the required OS
(an operator chose another, or findings name several OSes), a local drain, ship
discovery, `orbit run ship` and an owner's pull admission on a host that cannot
meet it leave the task in `backlog` as `native_os_required`; readiness,
`orbit run show` and the dashboard Drain card name the criterion and the tag.

When only the machine that owns the task store can produce the evidence (its
live store, services or data), the pilot records a `required_machine` finding
naming the criterion and that owner machine, committed in the same hold. The
owner then refuses a pull claim from any other machine with a reason naming
the machine; its own drain may start the task. A requirement on any other
machine stays a `utility_warnings` finding for an operator to arrange.

A claimed leaf that finds its base already resolves such a task hands off
`NoDiff` with an `already_resolved` outcome naming `cargo test -p <crate>
--test <target> [<filter>]` or `cargo test -p <crate> --lib [<filter>]`
selectors. Its claim host replays each on the base
outside the agent sandbox and pins the passing logs for the owner, so the task
settles without a PR; a failing replay fails the claim with its output, and an
empty list or a selector that runs no test is refused.

When only the deployed build on a named host can evidence a criterion, after
the change merges and is deployed there (that host's live service latency, the
installed binary against its live store), the pilot records a `post_deploy`
finding naming the criterion, the host and the exact measurement command,
committed as `post_deploy` in its assessment audit.

It is separate from
`required_machine` and holds no admission: running on that host would still
measure the deployed build, not the candidate. A criterion is typed in one of
the two, never both. The finding only lets the implementer hand the criterion
off as owed after deploy (see the task-execution reference); measurements the
candidate can repeat, such as timing the worktree's own binary, are ordinary
criteria.

Write such work as two kinds of criteria. Candidate criteria are measured by
the implementer with its own build: execve or request counts, payload size,
CLI timing of the worktree build, a test server started from the candidate.
Post-deploy targets are stated as such and name the host and the exact
measurement command, for example "After deploy, on `<host>`: `<command>`
reports p95 <= 150 ms over 20 paced samples". Only the second kind can be
owed.

Only typed findings route work: a host requirement stated in warning prose does
not. Platform mentions, cross-compilation targets, mocked checks, negative
tests and measurements any host could repeat are not findings.

The wait clears when the matching `os:` tag is added (admission then routes the
task by its tags), when the acceptance criteria are re-scoped, when a newer
assessment carries no such finding, or through the same evidenced human
decision as above, which records `native_os_requirement_resolved`.

A trusted native-host decision stays valid across later pilot assessments with
the same acceptance-criteria digest, OS requirement set and machine requirement.
Requirement order does not matter. Changed criteria or requirements supersede
the decision. Operator-validation and host-operational decisions remain scoped
to the assessment they answered; a new assessment supersedes them.

### User-facing UI

When a task's criteria or context files are UI a user sees, the pilot records
`user_facing_ui: true`, and the same atomic write adds the `ui` tag to a task
that lacks it. `false`, null or an absent field adds nothing, and the pilot
never removes a `ui` tag; any other value is refused for repair. The
task-authoring reference (UI tag) defines the tag.

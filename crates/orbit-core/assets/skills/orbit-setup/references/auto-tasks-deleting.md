# Auto-tasks: deleting a definition and plugin-seeded definitions

## Contents

- [Deleting a definition](#deleting-a-definition)
- [Definitions a plugin seeds](#definitions-a-plugin-seeds)

## Deleting a definition

`toggle off` pauses a definition and keeps it listed. `delete` removes it: the
YAML file, its scheduler cursor, and — for a `deliveries_landed` definition —
its consumer state. Every delete writes an audit record naming who deleted
what and the optional `--reason`. A delete that fails leaves the definition
and its cursor in place; if it failed releasing a pinned ref, the consumer
reset before it stays applied, recorded as its own audited reset.

- It refuses while a task minted from the definition is still open and names
  those tasks. Finish or close them, or pass `--force`; a forced delete leaves
  the open tasks as they are.
- A delivery consumer is torn down through the audited `reset` (see [below](auto-tasks-delivery.md#what-reset-does)),
  and every pinned `refs/orbit/automation/*` ref it held is deleted. Delete
  therefore refuses whenever that reset would — an executing action, or a
  consumer owned by another machine — and points at
  `orbit auto-task reset <name>` to preview it. `--force` abandons an
  executing action, exactly as it does for `reset`.
- Deleting one of the shipped defaults [below](auto-tasks-catalog-reviews.md#the-21-seeded-definitions) records an opt-out in
  `.orbit/auto_tasks/.orbit-managed-assets.json`. `orbit workspace init
  --force` and `orbit workspace sync` leave an opted-out default absent, and
  `orbit doctor` does not report it missing. Use this to drop the code
  sweeps from a workspace whose repository holds no code.
- `orbit auto-task restore <name>` is the way back for a deleted shipped
  default. It writes the shipped content, with the shipped `enabled` value
  (disabled for every default, including `host-health`), and clears the
  opt-out, so later reseeds manage it again. `restore` refuses a name Orbit
  does not ship and a definition that already exists. `auto-task add` under a
  deleted default's name creates your own definition instead; the opt-out
  stays, so reseeds never overwrite it.
- To keep a whole workspace to a chosen set of shipped defaults, set
  `seeding.auto_tasks` (and `seeding.routines` for routines) to `"none"` or
  a list of names instead of deleting them one by one. Every seeding path,
  upgrades included, honours it; narrowing it retires the left-out defaults
  Orbit seeded and you never edited, and an edited one is preserved under
  `.orbit/.retired-managed/`. `restore` refuses a name the policy leaves out.

  Follow [Seeding policy](configuration-layers.md#seeding-policy) to change an existing
  workspace's policy, preview sync and verify the result.
  `orbit workspace init --preset <file>` applies a policy, plugin-seeded
  enables and config keys from one reviewed file; the
  [configuration reference](https://orbit-cli.com/reference/config/#workspace-presets)
  has its format.

A user-authored definition records no opt-out: delete simply removes it.
`delete` and `restore` are not on MCP; over MCP (with `--toolset automation`
enabled), disable a definition instead, or use `orbit auto-task toggle <name> off`.

Required tools in a template extend an allowlist activity's baseline. In a
deny-list activity they cannot override `tool_disallow_list`. The implementing
activity (`agent_implement`) refuses to start when the list covers one.
`final_recovery`, `step_failure_recovery`, `pr_conflict_recovery`,
`agent_review_repair`, `review_reconciliation_review` and `task_pilot` drop that requirement from the requested tools and record a
note; the tool stays uncallable. Any other activity keeps the refusal.

They do not replace the list or bypass runtime capability, policy, filesystem,
subprocess, or authentication checks. Invalid, inactive, wildcard, or
non-agent-facing names fail dispatch before the provider starts.

The shipped `delivery-code-review` and `friction-curation` defaults require
`orbit.task.update` to persist their report or maintain task relations; `friction-curation` also requires `orbit.friction` action `update`, never the
operator-only `orbit.friction.resolve`. These declarations can hold follower
placement because task updates are denied in claimed mode and friction updates
are outside the closed claimed-owner bridge. Required-tool metadata grants no
authority over either restriction.

`qa-sweep` keeps its claimed path: its shared
requirements are `orbit.task.add`, `orbit.task.artifact` action `put`, and
`orbit.task.show`. It persists the execution summary with `orbit.task.update`
only on the owner; a claimed worker returns that summary for owner import, so
the template does not unconditionally require that tool.

`path-risk-review`
takes the same route for its creation-grant reauthorization: it runs the step
when `orbit.task.update` is available and otherwise leaves the filed task held
for the pilot, so it declares no grant and its placement stays unchanged.

A template that declares exactly `github.auth.status`, `github.run.list`,
`github.run.view`, `github.run.logs`, and `github.pr.list` is the worked
example. A minted instance therefore runs under
`effective_tools = activity baseline ∪ those five names` for allowlist mode.
The shipped deny-list `agent_implement` already includes those GitHub reads;
tasks requesting nothing keep that baseline.

Inclusion is only tool membership — a
structured `github.auth.status` answer may still report `available: false` or
`authenticated: false` when the lane has no GitHub CLI or no credentials.
That is unavailable evidence, not a clean CI result.

## Definitions a plugin seeds

An installed plugin may ship auto-task definitions. `orbit plugin enable <ns>`
seeds each one as `.orbit/auto_tasks/<ns>-<name>.yaml` with `enabled: false`
and a `# provenance: plugin:<ns>@<version>` header; a plugin may not ship a
definition that is already enabled. Review it, then `orbit auto-task toggle
<ns>-<name> on` like any other.

An upgrade re-seeds a seeded file only while it still matches what the plugin
wrote; a file you edited is preserved with a warning until
`orbit plugin enable <ns> --force`. While the plugin is disabled, removed, or
switched off in this workspace, the definition stays on disk and is skipped,
and listings hide it: `orbit auto-task list --all` shows it as `inactive` with
a reason naming the plugin, and `orbit auto-task show <name>` still resolves
it.

Tasks minted from one carry
`plugin:<ns>` beside `auto-task:<name>`, so their provenance survives the
plugin being removed.

<a id="the-twenty-three-seeded-definitions"></a>

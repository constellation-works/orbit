# Choose a crew and route a pull drain

## Crew filtering and pull owners

`--allow-crew` restricts a ship or drain to the crews you name — the lever for
a provider that is unavailable, rate-limited, or out of budget. For an
explicit ship it is checked at submission and again before provider dispatch;
for an auto drain it is opt-in and scoped to that run's window:

- Names must be crews this workspace configures. An unknown or empty one fails
  the command; nothing is dispatched, and no configuration is written.
- Scope is the run and everything it admits: the leaf pipelines it starts
  inherit the same restriction, and the check runs again at each activity
  against the crew that was *actually* resolved — including an activity that
  draws from a complexity pool — so an excluded provider cannot be reached
  through an alias. Matching is by effective configured identity, so a differently
  named crew resolving to the same provider/model is permitted; naming a wrapper
  is not itself provider usage.

  - Precedence is unchanged: explicit > task.crew >
    `[workflow].default_crew`, and the allowlist gates the winner rather than
    choosing one.
- A backlog task whose crew is excluded is **skipped, not remapped**. It stays in
  `backlog` on its own crew, and `orbit run readiness --allow-crew ...` reports it
  as `crew_not_allowed` with the crew it would have run as. Without `--allow-crew`,
  readiness applies the live drain's own set (`capacity.allowed_crews`). Moving that work to a
  permitted crew is an operator decision — reassign the task, then it drains
  normally. Everything permitted keeps filling the slots at the usual rate.
- It governs only what this drain *starts*. Tasks another invocation already has
  in flight keep running to completion; nothing is cancelled. It carries no
  completion or promotion authority, and there is no automatic fallback to a
  different provider.
- On a replica pull drain (`orbit run auto --pull <selector> --allow-crew ...`)
  it limits the crews the drain declares to the owner, on every pass and on
  resume, so the owner hands it only tasks on those crews. The owner's before-PR
  reviewer is not restricted but must still run on that host. A pull drain
  without `--for` makes one admission pass, then only settles.

For a pull drain, register the owner with `orbit host add <ssh-target>` and
check `orbit host list`. `--host <name-or-machine_id> --pull <workspace-name-or-ws_id>`
resolves the owner workspace from that host's live list. Without `--host`,
`--pull` takes only the full host-qualified selector copied from discovery;
do not compose one by hand. Run the drain from that owner's replica checkout.

The flag selects a pull owner, not the execution host for `run ship`,
`run show/history/logs` or other host-local commands. For task reads and
updates on the CLI or federated MCP, omit the workspace to route by task ID
prefix; task creation/listing and dispatch retain workspace selection.

See [tool-surface-routing.md](../../orbit/references/tool-surface-routing.md#task-ids-and-host-selection)
for the routed tools and error remedies, and
[distributed-drain-setup.md](../../orbit/references/setup/distributed-drain-setup.md)
for pull setup, captured review policy and owner landing authority.

Runs are asynchronous: [dispatch commands](orchestration-dispatch.md) return once the run is durable, printing a
run ID. They do not claim the eventual outcome.

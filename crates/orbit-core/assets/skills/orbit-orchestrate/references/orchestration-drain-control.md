# Retune, extend or stop a drain

## Resize, extend and stop

The [worker ceiling](orchestration-dispatch.md#drain-admission-and-capacity) is adjustable while the drain runs. `orbit run concurrency <run-id>
--set N` (MCP: `orbit_workflow` action `auto` with `mode: "resize"` and `concurrency`;
omit `id` to target the workspace's one live drain) records a live ceiling on the run
itself, so **do not cancel a drain to change how many workers it uses** — that
mints a new run id, restarts the window, and re-states the completion
authorization.

The retune keeps all of them:

- The next admission pass reads the new ceiling. Raising it fills the extra
  slots from the same backlog; lowering it stops new admissions until enough
  children finish, and cancels nothing that is already running.
- It is refused, with the reason, for a run that is not a drain (an `orbit run
  auto` window or a replica's `orbit run auto --pull` drain), has not started,
  or has already finished.
- `--if-revision N` makes the change conditional on the ceiling still being the
  one you read, so two operators cannot silently overwrite each other. The
  current value and who last moved it are on `orbit run show <run-id>`
  (`drain_worker_limit`) and `orbit run readiness`.

The window is adjustable the same way. `orbit run extend <run-id> --for <d>
[--concurrency N]` moves a live drain's deadline to the later of its current
one and now plus `<d>` (at most 24 hours ahead), on the same run, and retunes
its ceiling when `--concurrency` is given.

**Extend a drain whose window is
about to lapse instead of starting another**: a second `orbit run auto`
queues behind the live drain and admits nothing until that drain ends, which
for a pull drain is only after every leaf it claimed has settled.

- A shorter window is a no-op (`outcome: unchanged`); a window never shrinks.
- A pull drain can be extended in its settling tail, after its window closed,
  and resumes requesting. An owner drain whose window has closed is refused:
  it has stopped admitting.
- It is refused for a drain stopped with `--stop` or being cancelled (an
  extension never reopens one) and for a drain started without `--for`, which
  makes a single admission pass. A pull drain is refused too once a pass has
  closed its window with nothing left to settle: that drain is ending.
- The run ID, its completion authorization and every task in flight are
  preserved. `--concurrency` changes the ceiling as `orbit run concurrency
  --set` does, so a pull drain keeps the [bounded review credit](orchestration-dispatch.md#drain-admission-and-capacity).
- Each request is audited on the run with the old and new deadline, and
  `orbit run show` prints `Window: extended to …` (`drain_window_extension`
  in JSON). `orbit run readiness` reports the extended `ends_at`.

`orbit run auto --stop` ends new admissions for this workspace's active
coordinator. It does not need a run id, does not cancel children, and is
idempotent when nothing is running. `orbit run show` reports
`Admissions: stopped by ...` and lists remaining children. To cancel workers
already in flight, `orbit run cancel <child-run-id> --confirm` each one —
do not cancel the coordinator for this.

If you stop and restart instead of retuning in place, the replacement counts
those still-running workers against its new ceiling: with K inherited slots and
a ceiling of N, it admits at most N-K new tasks (zero when K exceeds N).
`run show` reports `Capacity: occupied=… inherited=… limit=…` (JSON:
`drain_summary.capacity`) from the last admission pass, before that pass starts
new work.

The `Leaves:` outcomes remain those of the displayed coordinator's
own children; inherited work is not counted as its success or failure.

An admission pass that times out on a busy task-store lock (`lock_busy`) is
skipped, not fatal: `run show` reads `Admission pass deferred:
consecutive_failures=N last_pass_error=...`, naming the lock's holder, and
the next pass retries after the poll interval. Ten in a row fail the drain
with that error; find the holder in `orbit.jsonl` before restarting it. Any
other admission error still fails the drain at once.

A drain's own `State: success` says the coordinator ran, not that its leaves
shipped: it dispatches them detached. Read the `Leaves:` line on `orbit run
show <drain-run-id>` (JSON: `drain_summary`) for admitted / succeeded / failed
/ running / cancelled counts. A leaf whose task is in `review` because its
newest delivery run ended `held` on a `delivery_*` refusal, or because a
`completion.hold` rule is still current, is counted in `held_in_review`
instead of `succeeded`.

That is not a failure: `failed` does not change.
With no such leaf the succeeded and failed counts stay as they were and the
printed line omits `held_in_review`. When the count is above zero, the card
prints `held_in_review=N`, a `Held in review:` breakdown (the delivery code
or the completion-hold rule, with the task ids) and one line per held task
naming the leaf run, the hold, the reason, the delivery run and the pull
request.

`--json` always includes `held_in_review`,
`held_in_review_by_reason` and `held_leaves`, including zeros. The card
lists each failed leaf with its `orbit job resume <leaf-run-id>`, and
a `Still waiting:` block for backlog tasks the last pass never started.

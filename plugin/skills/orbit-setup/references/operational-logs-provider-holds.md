# Operational logs: provider failure holds

## Contents

- [Provider failure holds](#provider-failure-holds)

## Provider failure holds

A local run can stop because its provider failed rather than the work. The
step error then carries a typed marker followed by `provider=<name>`. Only
the provider CLI's own stderr, terminal error, or failure frames set a
marker; an agent transcript or tool output that merely mentions a rate limit
does not. Treat this as read-only diagnosis: the hold lifts by itself, and
the next run resumes the kept candidate.

Do not clear state files, unblock
or replay the task, file a duplicate, or dispatch repeatedly to force it
through.

| Marker | Meaning | Run outcome | Excluded crews | Base backoff |
| --- | --- | --- | --- | --- |
| `[provider_capacity]` | The selected model was at capacity. | `held` (`provider_capacity`) | The run's crew, or, when the step ran on another provider or account, only the named account's crew (`account=<crew>`), or the provider's crews without their own credentials for a default-login failure. | 15 minutes |
| `[provider_unavailable]` | Unusable on this host, such as a failed login. | `failed`; the task is still held | The run's crew, or, when the step ran on another provider or account, only the named account's crew (`account=<crew>`), or the provider's crews without their own credentials for a default-login failure. | 30 minutes |
| `[provider_refusal]` | The provider's content policy refused the task. | `failed`; the task is still held | Every crew of the refusing provider. | 24 hours |
| `[provider_limit]` | The account hit a usage limit. | `held` (`provider_limit`) | The failing account's crews on that provider. If the failure names a model and at least one of them runs a model containing it (case-insensitive), only those crews; otherwise all of them. | Until the reported reset (below) |

Capacity, unavailability, and refusal backoffs double for each earlier hold
the task received in the last 24 hours, up to 24 hours. A usage-limit hold
lasts until the provider's reset, clamped between 5 minutes and 7 days;
with no readable reset it starts at 30 minutes and doubles per earlier
usage-limit hold on that provider account in the last 24 hours, up to
6 hours.

A crew that sets its own credentials is its own account: the marker then
carries `account=<crew>` after the provider, and a limit, capacity, or
unavailability failure excludes that crew alone. A failure without one is
the provider's default login and excludes only the provider's crews without
their own credentials. A refusal is the provider's content policy and still
excludes every crew of that provider.

An unavailability hold also names the
host's machine: a failed login is that host's, so its crews are excluded
only there, while crews an earlier still-standing capacity, limit, or
refusal hold excluded stay excluded on every machine. A claimed run on
another host that a provider refused releases its claim the same way, and
the owner records the hold from its own crews.

Until the release time, the drain gives the task to a crew the hold does
not exclude: the task's own crew or pool first, then the other enabled
members of its purpose or type pool, then its complexity pool, then the
workspace default. One case differs. Under the default
`workflow.provider_limit_explicit_crews = wait`, a usage-limit hold on the
task's explicit crew (`crew_source` `explicit`, or a legacy pin) does not
redraw it: the task waits in the backlog until the release time.

Under
`pool`, it is drawn from its complexity pool and then the workspace default,
as above. The draw source names the hold. With no crew left, the task waits
and `orbit run readiness` reports it with `reason: "provider_backoff"` and
the release time. A run started with an explicit crew input ignores the
hold. The hold ends at the release time or at any
later status change; do not move the task just to lift it.

The next run
resumes the committed candidate: a held run commits it on the run's branch
and pushes it to the candidate ref on the origin without opening a branch
or a pull request, so there is nothing to re-run or rebuild by hand.

Inspect the hold through these read-only surfaces:

```bash
orbit run show <run-id> --no-reconcile --json
orbit task show <task-id>
orbit run readiness <task-id> --json
```

The `--no-reconcile` flag reads stored run records without reconciling stale
runs or settling orphaned pull claims. The default `orbit run show` command can
reconcile a stale run, release its task reservations, and settle orphaned pull
claims, so it is not read-only.

`orbit run show` names the marker (`provider=<name>`, with
`account=<crew>` for a crew account), the diagnostic code, and whether the
run ended `held` or `failed`. `orbit task show` leaves the task in
`backlog` under a `provider_failure_hold` history event whose note names
the class, provider, excluded crews, release time, and run.

`orbit run
readiness` reports the waiting task with `reason: "provider_backoff"` and a
detail naming that run, class, excluded crews, and release time; eligibility
commands alone never clear it. Check `orbit run readiness --help`,
`orbit run show --help`, and `orbit task show --help` on the installed host
for the exact output shape before scripting around it.

Separate the hold from two lookalikes. A host outage from an
unavailability failure is recorded for this host only: until its backoff
lapses the local drain draws no task onto that crew, not only the held one,
and moving the held task through another status does not lift that. Sign
the provider in again on that host; tasks draw the crew once the outage
lapses.

An unavailability hold placed on another host excludes only the
crews carried from an earlier still-standing capacity, limit, or refusal
hold; those stay excluded on every machine. A provider usage-budget wait
instead reports `reason:
"provider_limit"` with a `Provider limits:` line naming each gated window,
its reset, and the skipped crews; `orbit doctor` warns on the provider's
`provider-limits` row. Nothing failed and nothing needs repair there: the
wait lifts by itself after the reset.

Thresholds and budgets are in
[configuration-settings-workflow.md](configuration-settings-workflow.md#common-settings). When the scheduled
task pilot withholds its dispatch, its reason names the provider or crew,
when the backoff ends, the failing run, and the skipped crews. After the
hold lifts, the pilot dispatches again with no operator action once the
routine's own debounce has passed; the first tick after the hold lifts
only starts that debounce.

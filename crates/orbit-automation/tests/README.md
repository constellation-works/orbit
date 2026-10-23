# Automation boundary coverage

`scheduling.rs` drives the public due-decision, scheduler and member evaluation
APIs with explicit timestamps. The due table includes Berlin's spring gap and
autumn fold (the daily cron selects the first fold occurrence), clock cadence
grace and actual downtime. The fixed-time spring-gap case records Croner's
backward-search resolution to the preceding real minute; wildcard schedules
cross the gap directly to the next real slot. Both must consume their slots once.
Interval checks cover anchored catch-up, consumed
slots, invalid periods and an unrepresentable future boundary.

State-mutating scheduler and coverage fixtures re-execute the exact test in a
child with inherited Orbit authority cleared, disposable home/workspace paths,
UTC host time and a 30-second deadline. The shared process supervisor drains
output and kills/reaps a timed-out child. Real stores and definition discovery
exercise cursors, overlap admission and coverage receipts; dispatch adapters
provide the lifecycle outcomes that Core normally supplies.
`scheduling/pending_mints.rs` seeds claims with and without recorded task ids
for enabled and disabled definitions. It compares dry-run and live reports,
checks that previews leave cursor bytes unchanged, and verifies that live
recovery consumes recorded mints while retaining unresolved claims without
reminting.
`scheduling/failed_members.rs` drives a member consumer through more than 1,000
distinct failed members that leave the source, beside a backlog of withheld
task ids, and shows every pass still commits and admits the next wave. It also
shows each failed member keeps only its own record of the attempt, that the
store refuses missing, outside, mismatched-attempt, sibling, forged-fingerprint
and whole-batch records (and one for a member the receipt certified), and that
a checkpoint whose records carry their whole batch and omit the later defaulted
`excluded` field resumes, commits a generation bump and is compacted.

The remaining `scheduling/` modules run in process. They use in-memory stores,
temporary definition roots and in-process fakes, and change no durable Orbit
or process-wide state.
`scheduling/definition_symlinks.rs` shows that auto-task and routine discovery
refuses a symlinked definitions directory (including `routines/local`) and
ignores a symlinked definition file. Auto-task catalog links are refused even
when their target is missing, so a broken local catalog cannot enable shared
fallback.
`scheduling/routine_sweep.rs` drives `run_sweep_core` with a scripted dispatch.
A dispatch that overruns the tick deadline defers the next workspace without
touching its cursor. An `overlap: forbid` slot whose run is marked interrupted
stays held while the owner is alive, is released once the owner stopped, and is
reclaimed at the policy timeout when liveness cannot be probed.
`scheduling/member_consumers.rs` shows a disabled member consumer without
state never probes its branch. It also shows the store refuses a checkpoint that
resets a frozen attempt's budget, replaces its acknowledged action or drops a
failure tombstone.
`scheduling/delivery.rs` holds the delivery fixture: a scriptable
`DeliveryHost` over an in-memory automation store. Its tests cover one admitted
action under concurrent evaluators, adversarial evidence that cannot
manufacture coverage, receipt failures that recover atomically and malformed
evidence that settles and spends the retry budget.
`scheduling/delivery_adopt.rs` shows a settings-only edit is adopted in the same
pass with every obligation retained. Edits that change what the debt means,
executing actions, consumers the evaluator cannot judge alone and hosts that do
not adopt all stay at `definition_changed`.
`scheduling/delivery_recovery.rs` covers operator recovery and reset. Refused
changes leave state untouched, and only a stopped action without acceptable
evidence is reissued. A terminal proof cannot reissue a concurrently admitted
replacement.

Materialization is owned by Core. Its migration coverage joins the existing
[`workspace_sync`](../../orbit-cli/tests/workspace/workspace_sync.rs) module of the CLI `workspace` integration binary
at the production CLI boundary, using the checked-in
[`v0.24.0` fixture](../../orbit-cli/tests/fixtures/automation-v0.24.0/README.md).
No production API visibility or crate dependency is changed for testing.

`review_validation/main.rs` drives the public review-coverage API with the
validation records a reviewer files. A superseded attempt with no same-identity
required pass anywhere in the report must fail closed at both consumers. `validation_evidence` gives
the gate's escalation reason, and `certificate_acceptable`/`exclusion` refuse
to spend a certificate whose `validation_complete` flag those records do not
support. The table covers missing, ambiguous (one-sided or cross-namespace
identity), invalid (blank or mismatched identity) and non-passing replacements.
Same-command reruns and shared check identities serve as the accepting controls
in both report orders, including replacements establishing captured host checks.
Different identities remain distinct even when a passing check claims broader
coverage and runs the same command.
It also covers an unrelated workspace failure filed as a scope-checked
`diagnostic`, a failed required check relabeled as a control or diagnostic, a
required check an earlier report revision recorded and the final records drop
or relabel, and the deliberate controls and resolved obligations that remain
coverage.

Focused commands:

```sh
cargo test -p orbit-automation --test scheduling
cargo test -p orbit-automation --test review_validation
cargo test -p orbit-cli --test workspace workspace_sync::workspace_sync_upgrades_previous_release_automation_with_provenance_intact -- --exact
```

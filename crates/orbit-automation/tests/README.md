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

Materialization is owned by Core. Its migration coverage joins the existing
[`workspace_sync`](../../orbit-cli/tests/workspace/workspace_sync.rs) module of the CLI `workspace` integration binary
at the production CLI boundary, using the checked-in
[`v0.24.0` fixture](../../orbit-cli/tests/fixtures/automation-v0.24.0/README.md).
No production API visibility or crate dependency is changed for testing.

`review_validation.rs` drives the public review-coverage API with the
validation records a reviewer files. A superseded attempt that no later required
pass replaced must fail closed at both consumers. `validation_evidence` gives
the gate's escalation reason, and `certificate_acceptable`/`exclusion` refuse
to spend a certificate whose `validation_complete` flag those records do not
support. The table covers missing, ambiguous (one-sided or cross-namespace
identity), invalid (blank or mismatched identity) and non-passing replacements.
Same-command reruns and shared check identities serve as the accepting controls.

Focused commands:

```sh
cargo test -p orbit-automation --test scheduling
cargo test -p orbit-automation --test review_validation
cargo test -p orbit-cli --test workspace workspace_sync::workspace_sync_upgrades_previous_release_automation_with_provenance_intact -- --exact
```

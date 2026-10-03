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
[`workspace_sync.rs`](../../orbit-cli/tests/workspace_sync.rs) integration binary
at the production CLI boundary, using the checked-in
[`v0.24.0` fixture](../../orbit-cli/tests/fixtures/automation-v0.24.0/README.md).
No production API visibility or crate dependency is changed for testing.

Focused commands:

```sh
cargo test -p orbit-automation --test scheduling
cargo test -p orbit-cli --test workspace_sync workspace_sync_upgrades_previous_release_automation_with_provenance_intact -- --exact
```

# Respect repair authority and activity boundaries

## Repair tasks and activity boundaries

Prefer a bounded task for actionable tooling, documentation, or operational
friction. A separate friction artifact is optional, not a prerequisite; honor
a user's preference to file tasks only. Avoid duplicate records that describe
the same fix without adding useful evidence.

Agent output is advisory, not an authoritative activity success contract. Do
not add generic output-schema enforcement or retries that force a model to
produce a particular report shape. Deterministic operations validate the
inputs they consume and the state they change at their own boundary. Verify
actual files, persisted selectors, tests, and delivery outcomes. Improve a
misleading prompt or diagnostic narrowly when evidence supports it.

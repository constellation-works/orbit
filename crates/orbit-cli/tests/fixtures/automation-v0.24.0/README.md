# Previous-release automation fixture

The YAML definitions come from Orbit `v0.24.0`, commit
`abca0ee670d1a4e633990974356e889fded97878`:

- `crates/orbit-core/assets/routines/task_pilot.yaml`
- `crates/orbit-core/assets/auto_tasks/delivery-qa.yaml`
- `crates/orbit-core/assets/auto_tasks/friction-curation.yaml`

Only materialization placeholders were substituted: the routine name is
`task-pilot-legacy-workspace`, its owner is `hm_workspace_sync`, and the routine
and delivery definition observe `main`. The manifests use the release's schema
versions (routine v2, auto-task v1), rendered SHA-256 digests and, for the routine, its
original template digest and materialization binding. The fixture contains only
these three entries to keep the upgrade test focused; sync creates other
defaults.

The `workspace_sync` integration test installs these on-disk catalogs into an
isolated registered workspace with a different name, then invokes the real CLI
with a bounded wait. It checks refresh and preserved bindings/digests, also with
a routine opt-in and a handwritten auto-task schedule, and checks that another
sync leaves the materialized definitions and manifests byte-for-byte stable.

These are input fixtures, not output goldens. Do not regenerate them from
current assets: their old bytes and digests are the migration contract. The unchanged
delivery
definition proves that sync keeps existing provenance, while the changed friction
definition proves that sync refreshes only unedited managed bytes.

---
type: design
summary: "Spec: Task Bundle V2"
tags: ["task-artifacts"]
last_validated: 2026-09-08
---

# Spec: Task Bundle V2

Task Bundle V2 defines the canonical on-disk contract for Orbit tasks after the schema reset. The contract is source-of-truth storage, not merely a rendering preference: implementations should expose the bundle shape below rather than maintaining long-lived compatibility with the previous task schema.

## Why This Exists

The current task bundle is easy to inspect but hard to sync and evolve. Long prose, append-heavy audit arrays, status directory moves, and workspace-local IDs all interact badly with shared registries and long-lived agent workflows. This spec pins the reset target so cutover work and new code converge on one artifact shape.

## Normative Language

`must` marks required behavior for conforming v2 readers and writers. `should` marks a recommended default that improves authoring or UX but is not a storage-level validity rule unless a transition policy explicitly enforces it.

## Bundle Path

Every canonical task bundle lives at:

```text
~/.orbit/tasks/workspaces/<workspace-id>/<task-id>/
```

`<task-id>` must be the canonical ID inside the current allocation authority. The canonical v2 format is `ORB-` plus a decimal suffix formatted with at least five digits (for example, `ORB-00000`); parsers accept wider decimal suffixes and the allocator is bounded by `u32::MAX`. `<workspace-id>` is assigned once per workspace as `<slug>-<6char>` and stored in `.orbit/config.yaml`. Old `T<YYYYMMDD>-<N>` IDs are not valid v2 identifiers or lookup aliases.

Required files:

```text
task.yaml
description.md
acceptance.md
plan.md
execution-summary.md
events.jsonl
comments.jsonl
```

Required directories:

```text
artifacts/
```

`artifacts/manifest.yaml` is required when `artifacts/files/` contains any files.
Legacy `review-threads/` directories are inert sidecars: readers ignore them
when present and do not require, create, remove, or quarantine them when absent.

New bundles must be assembled and validated in a unique sibling staging
directory, including all manifest-referenced artifact blobs, then published by
an atomic directory rename. An interrupted create must leave the canonical task
path absent rather than expose a partial bundle.

## Local Store and Canonical Lookup

The home-directory bundle is the active source of truth for task content. Local-first Orbit must keep allocation and local operational metadata under:

```text
~/.orbit/tasks/index.sqlite
```

Canonical workspace bundles live under:

```text
~/.orbit/tasks/workspaces/<workspace-id>/<task-id>/
```

The index must record allocation authority state, canonical IDs, workspace bindings, materialized bundle paths, local execution bindings such as `workspace_path` and `repo_root`, generated status/relation/tag index rows, and task-lock reservation bindings or pointers.

Each workspace must have `.orbit/config.yaml` with at least:

```yaml
schema_version: 1
workspace_id: orbit-a3f9c2
```

Task and artifact tools must resolve bundles from the workspace binding and canonical registry path. Task mutations must make the canonical bundle and registry metadata durable before reporting success. They must not create a checkout-local task-bundle projection. If `.orbit/config.yaml` is missing, Orbit must prompt to rebind by matching the checkout path, repo root, and optional remote fingerprints against `index.sqlite`; ambiguous matches must not silently attach to a workspace.

Delete publishes a canonical-bundle tombstone, unregisters the task binding and generated index rows, then removes the tombstone. Legacy checkout task links are handled only by the guarded workspace-layout upgrade, which must never follow them or remove ambiguous entries.

## Envelope

`task.yaml` must contain only structured metadata:

```yaml
schema_version: 1
id: ORB-00000
title: Short title
status: proposed
type: chore
priority: medium
complexity: null   # legacy unlabeled only; new creates always persist a value
job_run_id: null
crew: implementer-crew
orchestrator: orchestration-crew
relations:
  - type: resolves
    target: F2026-05-007
tags: []
context_files: []
external_refs: []
created_by: codex:gpt-5.5
planned_by: null
implemented_by: null
created_at: 2026-05-11T00:00:00Z
updated_at: 2026-05-11T00:00:00Z
```

The envelope must not contain `description`, `acceptance_criteria`, `plan`, `execution_summary`, `history`, `comments`, or `review_threads`.
The envelope must not contain local-only `workspace_path` or `repo_root`; those live in the local registry workspace binding.
The envelope must not contain the old `batch_id` name or internal execution-routing fields such as `agent` and `model`; job-run membership is stored as `job_run_id`, while durable task attribution uses `created_by`, `planned_by`, `implemented_by`, and the explicit `orchestrator` named-crew alias. `crew` is execution selection only; `orchestrator` is read-only orchestration attribution, never a model/provider selector. It is mutable only in `proposed` and `backlog`, and a missing field in a schema-v1 bundle reads as `null`. A legacy writer can omit this additive field, so schema-v1 compatibility guarantees readability rather than preservation across legacy writes.
Task-only relation types must target `ORB-` IDs. `produces` and `resolves` may additionally target friction and ADR IDs.

## Documents

Document sidecars are UTF-8 Markdown:

- `description.md` may be empty only for machine-generated placeholder tasks.
- `acceptance.md` should have at least one bullet or checkbox before a task enters `in-progress`; storage does not define a waiver list.
- `plan.md` must be non-empty before entering `in-progress` when the transition policy requires a plan.
- `execution-summary.md` must be non-empty before entering `review`.

Convenience reads:

- `description` returns `description.md` as a string.
- `acceptance` returns `acceptance.md` as a string; UI code may parse checkboxes for display.
- `plan` returns `plan.md` as a string.
- `execution-summary` returns `execution-summary.md` as a string.

## Events

`events.jsonl` is append-only. Each line is a JSON object with:

- `schema_version`: event row schema version, initially `1`.
- `event_id`: stable per-task event ID.
- `at`: RFC 3339 timestamp.
- `by`: actor label.
- `type`: event type.
- `note`: optional string.
- `from_status`: optional status.
- `to_status`: optional status.

Writers must not rewrite prior event lines except during explicit cutover or repair commands. Appends must write one JSON object plus newline, flush and sync the file before success, and preserve a valid prefix if the process crashes. Readers must tolerate a final unterminated or invalid JSON line as tail corruption; repair may truncate only that final corrupt tail.

### Notes must not inline bulk

`note` is a human-readable summary, not a payload channel. A writer must not
inline a machine-generated blob — a serialized structure, a path list, a
captured stream — into it. Because `events.jsonl` is append-only, an oversized
note is permanent: it dilutes every downstream reader of that task (history
scanning, memory distillation, triage, and the agent task context assembled in
`v2_host/task_context.rs`) for the life of the record.

A writer whose note would carry such a payload must instead keep a leading
excerpt and name, **inside the note itself**, the command that reads the full
text back. Eliding is only permitted where the full value is independently
durable: the note carries a copy, and the copy is what gets dropped. Discarding
a value that exists nowhere else is not permitted at write time — the note is a
durable record and a lossy write cannot be undone later.

The one such elision today is the `workflow_run_failed` note, whose
`error_message` is retained in full by `job_run_steps.error_message` and read
back with `orbit run show <run_id> --json` (field `.run.steps[].error_message`).
Its size threshold is declared exactly once, in `orbit-engine`'s
`context::outcome`, and `scripts/check-history-note-size.sh` fails the build if
a second threshold or a second `workflow_run_failed` note producer appears.
Re-measure the surface with `scripts/measure-history-signal.py`. [ORB-10343]

## Comments

`comments.jsonl` is append-only. Each line is a JSON object with:

- `schema_version`: comment row schema version, initially `1`.
- `comment_id`
- `at`
- `by`
- `body`

Comment bodies are Markdown strings. Multi-line bodies must be JSON escaped inside one JSONL row, not spread across lines.
Comment appends follow the same atomicity and tail-repair rules as `events.jsonl`.

## Retired Review-Thread Sidecars

The `orbit.task.review_thread.*` surface was retired in [ORB-10332]. Existing
`review-threads/` directories may remain for forensic inspection, but they are
not part of bundle validity. Recovery is non-destructive: ordinary reads,
lists, searches, and creates neither delete nor silently recreate those
sidecars.

## Relations

Relations are directed entries stored in the task envelope. The initial relation type set is:

- `blocked_by`: source task is blocked by target task.
- `child_of`: source task is a child/subtask of target task.
- `spawned_from`: source task was created from target task.
- `regression_from`: source task tracks a regression introduced by target task.
- `supersedes`: source task replaces target task.
- `related_to`: source task is associated with target task without stronger semantics.
- `produces`: source task created a task, friction, or ADR artifact.
- `resolves`: source task closed or superseded a task, friction, or ADR artifact.

Writers must validate the relation type set, reject self-edges, reject duplicate `(type, target)` entries on one source task, and reject cycles for hierarchy and blocking relation families. Relation types are source-implied: create-subtask writes only `child_of -> parent` on the child, and create-dependent-task writes only `blocked_by -> dependency` on the blocked task. Inverse lookup is generated from indexes, not stored as peer records.

Local indexes should materialize `(source_task_id, relation_type, target_task_id)` and inverse lookup rows so lineage queries do not scan every task bundle.

## Generated Views

The local registry must maintain generated status and terminal-month views or rebuild them from bundles/events on demand. These views replace the current `review/` and `done/<yyyy-mm>/` directory browsing affordances without making lifecycle state part of the path.

The initial registry projections are:

- `task_bundle_index(task_id, workspace_id, status, priority, job_run_id, created_at, updated_at, terminal_month, complexity)`. `complexity` is `low`/`medium`/`hard`, or empty when the envelope left it unset. SQL `NULL` means the row has not been rewritten since the column was added.
- `task_bundle_tags(task_id, workspace_id, tag)`.
- `task_bundle_relations(source_task_id, workspace_id, relation_type, target_task_id)`. The physical column name is historical; `produces` and `resolves` rows may store non-task artifact IDs in `target_task_id`.

Indexes are generated data. The bundle envelope is canonical, and repair/rebuild paths may delete and regenerate index rows from bundles. The `task_bundle_index.updated_at` value is the envelope version stamp. Query paths should treat a missing row, incomplete index, or `updated_at` mismatch as a cache miss: rebuild from registered bundles when possible, otherwise fall back to bundle reads rather than treating the index as proof that tasks do not exist.

## Local Locks

Task lock reservations are not task artifacts. They remain local operational state in SQLite, keyed by workspace binding and canonical task IDs, with TTL/release semantics and audit events. File-overlap checks remain the conflict mechanism for actual work exclusion. During cutover, v2 reservations carry `workspace_id` alongside the legacy `.orbit` path so cleanup can still find older path-scoped rows; a later old-store removal can make `workspace_id` mandatory. In v2 mode, a missing `.orbit/config.yaml` workspace binding is an error at reservation write time.

## Crash Consistency

The file bundle does not provide all-or-nothing transactions across Markdown sidecars, JSONL logs, artifacts, envelope metadata, and generated SQLite indexes. The supported failure model is detect-and-repair:

- `task.yaml` remains canonical for structured metadata.
- JSONL tail corruption is repaired only at the final partial row; corruption before the tail is an error.
- The last event with `to_status` must match `task.yaml.status`; a mismatch on a *settled* bundle is corruption and must fail reads. A mismatch observed while a writer is mid-publication is not corruption and must not be observable at all — see [Concurrent reads and lifecycle writes](#concurrent-reads-and-lifecycle-writes).
- Generated indexes are invalid when count or `updated_at` stamps differ from registered bundle envelopes and must be rebuilt from bundles.
- Artifact manifest entries must reference existing relative files with matching size and SHA-256; unmanifested files are ignored until a future compaction/prune command removes them.

Malformed registered bundles surface as a typed `task_bundle_corrupt`
diagnostic containing the task ID, canonical path, and reason. Direct lookup of
another ID and allocation/publication of a new task must not scan the malformed
bundle. List and search remain fail-loud and return that diagnostic. None of
these query paths may delete, move, repair, or quarantine bundle bytes.

### Concurrent reads and lifecycle writes

A lifecycle write publishes across more than one file — a status transition
appends to `events.jsonl` and then republishes `task.yaml` — so between those
two steps the bundle on disk has an event log the envelope does not yet agree
with. That intermediate state is indistinguishable on inspection from the
settled mismatch the rule above calls corruption.

Readers must therefore observe the writer's coordination rather than infer
intent from bytes:

- A writer holds that task's exclusive bundle lock (`<bundle>/task.yaml`, via
  its sibling lock file) for the whole multi-file publication.
- A reader that assembles a **complete** bundle must hold the same lock in
  shared mode. It then observes only settled bundles, so a genuine mismatch
  still fails the read (`task_bundle_corrupt`) with no tolerance widened.
- The lock is per bundle: one task's transition must never block, fail, or
  delay a read of any other task.
- Reads of the **envelope alone** need no lock. `task.yaml` is renamed into
  place atomically, so a single file is always self-consistent; the index
  freshness scan every listing performs stays lock-free.
- The read lock is not the create/delete sentinel, which keys on the bundle
  directory. Readers never block a task's creation or removal, and those
  transient states stay governed by the existing skip tolerance
  (missing directory or sentinel held).
- Acquisition is re-entrant per thread, so a writer that reads the bundle it
  is about to modify from inside its own critical section does not deadlock,
  and it is best effort, so a store on a read-only mount still serves reads.

This is coordination between live processes, not a file-format transaction: it
adds no on-disk state and changes nothing about post-crash recovery, which
remains detect-and-repair. [ORB-11349]

## Artifacts

Artifacts are stored under `artifacts/files/` and listed in `artifacts/manifest.yaml`:

```yaml
schema_version: 1
files:
  - path: reports/result.json
    blob: files/reports/result.json
    media_type: application/json
    sha256: "<64 lowercase hex chars>"
    size_bytes: 1234
    created_by: codex:gpt-5.5
    created_at: 2026-05-11T00:00:00Z
```

Artifact paths must be relative, UTF-8, slash-separated, canonical paths and must not contain `.`, `..`, or leading `./` components. Writers that ingest hand-authored manifests should normalize leading `./` before validation. `sha256` must be a 64-character lowercase hex SHA-256 digest; writer code should format digest bytes with lowercase hex (`{:x}`), not uppercase.

The bundle format does not guarantee cross-file transactions *across a crash*. Writers must keep single-file updates atomic and keep post-crash partial multi-file states readable; generated repair/indexing commands reconcile cases such as appended events before envelope status rewrite or artifact files written before manifest rewrite.

A *live* writer is a different case, and readers must not be exposed to its intermediate states. See [Concurrent reads and lifecycle writes](#concurrent-reads-and-lifecycle-writes).

## Cutover

Cutover from the current pre-reset task schema must:

1. Create or reuse `.orbit/config.yaml` with a stable `workspace_id`.
2. Allocate a canonical `ORB-00000` ID.
3. Record allocation and workspace binding metadata in `~/.orbit/tasks/index.sqlite`.
4. Materialize the canonical bundle under `~/.orbit/tasks/workspaces/<workspace-id>/<task-id>/`.
5. Move YAML `description` to `description.md`.
6. Render YAML `acceptance_criteria` into `acceptance.md`.
7. Preserve existing `plan.md`.
8. Preserve existing `execution-summary.md`.
10. Convert YAML `history` to `events.jsonl`.
11. Convert YAML `comments` to `comments.jsonl`.
12. Preserve any legacy review-thread files as inert sidecars.
13. Rewrite `task.yaml` with schema version 1 and no old ID aliases.
14. Rewrite or release active task-lock reservations.
15. Record generated status, terminal-month, relation, tag, and semantic-index rebuild inputs.

Cutover must be idempotent for interrupted local runs. A partially converted task must either repair cleanly on rerun or fail with a diagnostic that names the task ID and incomplete step. The command may emit an old-ID-to-new-ID report for humans, but that report is not a persisted lookup contract.

## Agent Signature

Last revised by `claude` on 2026-08-09 for [ORB-10343].

## Bounded list reads

ORB-11205: bounded task queries validate the generated index against every
registered, settled envelope, then apply metadata predicates and newest-first
ordering (task ID ascending for ties) before loading bundles. Exact totals
count metadata matches; they do not certify off-page body, event, or artifact
integrity. A selected bundle with envelope, body, or event-log damage fails
the request and is never replaced with another row. Listing and search
materialization use a locked lightweight bundle read: they still apply the
canonical bundle lock and event/envelope status consistency, but they do not
open or hash `artifacts/files/**` payload bytes. Direct `get` reads, explicit
reindex, import, publication restore, and artifact retrieval remain strict
full-bundle verification.

Status, type, priority, parent, job run, tags and external-reference predicates
use metadata. Readiness and context-path predicates currently use an explicit
residual fallback, hydrating metadata matches before filtering and limiting.
Missing/stale indexes require a lightweight bundle scan (task fields only) and
best-effort index repair; task-field errors encountered reading that scan
propagate. An update racing selected-row hydration causes one rescan with
filter-before-limit semantics. In-flight creation/deletion retains the
existing list-read tolerance.

Dashboard list and detail projections retain comments, history and the sorted
artifact manifest from each validated bundle. The aggregate selects the global
newest 50 from workspace metadata before hydration and shares one request-scoped
global dependency-status projection. Storage and rendering run on the blocking
pool, including cold workspace selection and runtime construction in the
shared workspace extractor. Envelope validation and dependency-status work remain linear in corpus
size; there is no persistent validation cache or content integrity audit added.

### Reproducing the bounded-read measurements

The ignored `task_list_io_benchmark` store test generates three temporary
workspaces, each containing `ORBIT_TASK_BENCH_SIZE` tasks (100, 1000 or 10000).
Each task has a roughly 8 KB description, a nonempty plan, eight comments,
twelve history events and a 1 KB artifact. Every tenth task has the selective
tag. It measures unfiltered and selective limit-50 reads, detail, and the
global newest-50 aggregate. `ORBIT_TASK_BENCH_MODE=baseline` uses the frozen
settled-index read algorithm from `424529c518d59631bb55e1df579454ff5e10307a`;
`candidate` uses the bounded store query. Counters distinguish full bundle
loads from envelope-only freshness reads (each full bundle also reads an
envelope). Residual and rebuild costs are covered separately by the listing
regression tests.

For example, after building test binaries outside the checkout:

```sh
export CARGO_TARGET_DIR=/tmp/orbit-task-bench-target
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
ORBIT_TASK_BENCH_SIZE=1000 ORBIT_TASK_BENCH_MODE=candidate \
ORBIT_TASK_BENCH_ROOT=/tmp/orbit-task-bench-1000 \
cargo test -p orbit-store task_list_io_benchmark -- --ignored --nocapture

ORBIT_TASK_BENCH_SIZE=1000 ORBIT_TASK_BENCH_MODE=candidate \
ORBIT_TASK_BENCH_ROOT=/tmp/orbit-task-bench-1000 \
cargo test -p orbit-web task_response_benchmark -- --ignored --nocapture
```

The fixture root must be new and temporary. Omitting it from the store test
automatically removes the generated corpus after the measurement. To compare
actual HTTP implementations, archive the baseline commit into a temporary
directory and add only `api/tests/task_response_bench.rs` and its test-module
registration. Run that identical harness on the same retained corpus, setting
the mode label to `baseline`. Build both binaries first, then run benchmarks
serially without compilation overlap. HTTP measurements include response
serialization and an unrelated workspace request under four concurrent lists
on one Tokio worker.

Both harnesses warm each operation before eleven timed samples, reporting the
median and nearest-rank p95 (the maximum with eleven samples). Linux `VmHWM`
reports process peak RSS including fixture/runtime setup and earlier cases;
it is not a per-request allocation measurement. The recorded ORB-11205 run
used unoptimized test binaries with debug information disabled and temporary
corpora on Linux tmpfs. These are warm-cache measurements, not controlled
cold-cache or production-release latency claims. Small-corpus pool/metadata
overhead and concurrent-request RSS must be reported alongside improvements.

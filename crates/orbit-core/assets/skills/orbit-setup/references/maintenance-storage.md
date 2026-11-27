# Maintenance: stores, retention and evidence

## Contents

- [Audit events](#audit-events)
- [Store retention](#store-retention)
- [Admission receipt storage](#admission-receipt-storage)
- [Logs](#logs)
- [GitHub run-log cache](#github-run-log-cache)
- [Search indexes](#search-indexes)
- [What is evidence and must not be edited](#what-is-evidence-and-must-not-be-edited)

## Audit events

```bash
orbit audit list --since 1h --status failure
orbit audit stats --since 7d
orbit audit export --output audit.json
orbit audit prune --older-than 90d --confirm
```

The audit store is persistent invocation metadata: who called what, when, and
whether it was denied. It grows without bound until pruned. Prune requires
`--confirm`; export first if the history matters.

## Store retention

The host store (`~/.orbit/orbit.db`) also keeps every run's audit events and
pipeline state, and the commit decision of every task transition (the task
commit journal). Each workspace keeps the blobs those events name under
`.orbit/state/audit/blobs/`. Three commands bound them. Each only reports
until you pass `--confirm`:

```bash
orbit gc audit                        # rows and bytes past retention.audit_days, and unreferenced blobs
orbit gc audit --confirm              # delete them
orbit gc runs                         # terminal runs past retention.runs_days that still keep pipeline state
orbit gc runs --confirm               # drop that state; the run, its steps and summary stay
orbit gc journal                      # applied commit journal rows older than 14 days
orbit gc journal --confirm            # delete them; unsettled rows and each task's newest stay
orbit gc audit --older-than-days 30   # one-off window instead of the configured one
```

`retention.audit_days` and `retention.runs_days` default to 60 days. On a large
store, consider `retention.runs_days = 30` before enabling the sweep. Values
below 30 are refused for new config writes and command overrides because
Reliability Pipeline echo dedupe and forecast-accuracy limiter shares need
30 days of run state. Older global or workspace files with a shorter window
load with an effective 30 days and a warning; startup leaves the file intact.

Persist the correction with `orbit config set retention.runs_days 30` (add
`--global` for the global file). Leave audit retention at 60 unless you have a reason to shorten it. `gc audit` prunes the host-wide command audit,
this workspace's run audit, and run audit whose workspace id is no longer
registered, then removes blobs that no remaining audit row, run step or
pipeline state names. Another registered workspace keeps its own window.

A blob
written in the last 24 hours, or one a write in progress has marked as pending,
is always kept. `gc runs` never touches a held run or one still in flight, so
`orbit run show`, run history and the Cost page keep working, and it reaches
the same unregistered workspace ids.

The same pass prunes the host's
per-minute run and host resource history (`run_resource_minutes`,
`host_resource_minutes`) and admission forecasts older than 30 days, whatever the
run window, except the rows its counts keep at any age: each workspace, work class
and complexity's newest 200
deliveries, the newest 10,000 forecasts, and host minutes from the oldest kept
forecast onward.

Auto-concurrency profiles and forecast accuracy keep working on
those rows; older raw samples outside them are lost.
Durable lock-denial facts and commit outcomes preserve Lock contention and succeeded implement_one durations after pruning; upgrade backfills
existing evidence before GC. Health Events, Incidents, Denials, `orbit audit` and
the diagnostics friction view lose raw history past the audit cutoff with no
rollups.

Task-store Codebase rankings, `orbit task flow` and friction stats keep
working. See the retention runbook for each setting's history boundary.

`gc journal` deletes applied task commit journal rows of this workspace, and of
unregistered workspace ids, older than 14 days.
Recovery replays only prepared and committed rows; an applied row is a decision
already published to its task, kept only to diagnose recent transitions. So
prepared, committed and aborted rows stay, and so does each task's newest
applied row.

The window is shorter than the others because the journal grows
fastest: at about 600 rows of about 80 KB a day, 60 days would pass 2.5 GiB. Its
rows are selected by one read that takes no write lock, then deleted by key in
batches. The `store-gc` routine does not run it; apply it yourself.

Deletes run in batches of 1,000 rows, each its own short write transaction, so
running workers are not starved. Freed pages stay inside the database file; each
report shows the freelist, and `VACUUM` returns it to the filesystem. Run
`VACUUM` yourself while nothing is using Orbit:

```bash
sqlite3 ~/.orbit/orbit.db 'VACUUM;'
```

`orbit doctor` reports what audit and run retention could reclaim now
(`store-retention`); it does not count journal rows. It warns
when the store file is larger than 1 GiB
and no store-gc routine is enabled, and it names the enable command. To run the
audit and run sweeps daily, enable the seeded `store-gc` routine ([automation-scheduler.md](automation-scheduler.md)).
On a replica checkout the same cron routine runs against that host's own store
and does not call the owner.

Enable it there when this machine is the only
place those rows live. A delivery- or state-triggered copy stays owner-only.

## Admission receipt storage

Workspaces that admit tasks for replica checkouts keep admission request
history apart from ordinary store garbage collection. Logical payload
accounting and permanent replay tombstones are not file-size reclaim; for
file size, see Store retention above.

```bash
orbit doctor admission-receipts          # read-only inspection
orbit doctor admission-receipts --json
```

Run the inspection on the owning checkout, or through the owner's federated
selector. A replica checkout refuses owner tools and names the owner to
contact instead. The report carries three separate totals, each with a count
and logical payload bytes: full admission receipts, permanent tombstones, and
claim-mutation receipts. Logical bytes count UTF-8 JSON payload lengths only;
they exclude row keys, indexes, free pages and write-ahead log state, and are
not filesystem usage.

The read-only inspection refuses while a commit is pending recovery rather
than repairing state. Recover first, then re-run the inspection.

Compaction is explicit maintenance, never automatic: admitting tasks does not
scan receipt history, so idle payloads accumulate until an operator runs it.
Capture the read-only report before and after to verify the change. Run
compaction only when deliberately authorized:

```bash
orbit doctor admission-receipts --compact
```

Under the exclusive admission boundary it first recovers pending commits,
then replaces eligible full receipts with permanent tombstones. A tombstone
keeps only the originating machine identity, request identity and input
digest in the same persisted encoding as replay. Eligible are idle receipts
with no claim and receipts whose claim has settled, except failed or revoked
claims that kept a pull request: those keep their full receipt to authorize
later external-merge settlement.

Unsettled claims — claimed, running,
handed-off and repair-pending — keep their full receipts, as does a receipt
whose claim record is missing. Reservation expiry and age never count as
settlement, and there is no age-based tombstone deletion.

Replay stays refused: an exact replay of a tombstoned request reports expiry,
and a replay with changed input reports a mismatch; neither admits again.
Repeated runs are idempotent and report how many receipts were compacted plus
the resulting storage snapshot.

Claim-mutation receipts are reported alongside but pruned separately: they are
kept for seven days after settlement and this command does not run that
pruning. Never delete receipts by age or edit the store directly; use only the
inspection and `--compact` above.

## Logs

The global JSONL trace at `~/.orbit/state/logs/orbit.jsonl` rotates from
long-lived processes (`orbit mcp serve`, `orbit sweep`, `orbit web serve`)
and when the active file exceeds its budget (one `metadata()` check on first
write). Short-lived commands, including `orbit --help`, do not open the file
or walk the log directory. These budgets are fixed in code: seven days of
archives, a 500 MiB total budget, a 100 MiB active-file threshold. They are
not tunable.

The former `runtime.log_retention_days`,
`runtime.log_max_total_mb` and `runtime.log_max_file_mb` keys are ignored
when present (recognised only until 2026-12-08); delete them from
`config.toml`.

```bash
orbit log tail -n 120
orbit log tail --level warn --since 1h
```

For diagnosing a host-level incident rather than tuning retention, see
[operational-logs-triage.md](operational-logs-triage.md).

## GitHub run-log cache

`github.run.logs` keeps the log of a completed job or run on the host, so
paging one log in byte or line windows (or by step) downloads it once instead
of once per window. It has no settings; its limits are fixed.

- **Location.** `<Orbit data root>/state/github-run-logs/` (`$ORBIT_ROOT`
when set, else `~/.orbit`), a `0700` directory of `0600` files. Each entry is
written through a staging file and renamed into place, so a partial download
is never read as an entry. A sibling lock file beside the directory
serializes publishes and removals; a removal only deletes the file it
classified, so a concurrent fresh entry under the same name is kept. Entries
are named `<sha256>.log`. An older unredacted directory is never read and is
removed when the cache is next opened or an agent sandbox is prepared.

- **Redacted before storing.** Before any byte reaches the disk, staging files
included, every span the response redactor would rewrite is overwritten with
`*` bytes of the same length, so byte and line coordinates stay those of the
served log. A log whose secret is found only across lines is not cached.
Every read of a completed log, the read that fills the cache included,
renders from that masked copy, so a cold and a warm read return the same
output. Only complete logs are stored; a partial stream is never kept.

- **Read rules.** Every read first asks the hosting service for the job's or
run's status under the caller's current credential and checkout. The cache
serves only when that request succeeds and reports the job or run completed.
Keys use the host and repository the status answer reports, never the
request's repository text or placeholders: a job-scoped read keys on run, job
and scope; a run-scoped read keys on run, run attempt and scope, so a rerun
never serves an earlier attempt's log; the job-log fallback keys on run and
job, not scope, and is kept apart from the run-scoped entry for the same job.
When the run-scoped read prints nothing, is refused as not yet complete, or
fails because the run's log archive arrives as an invalid zip, and the tool
recovers from a completed job's own log, that whole-job log is cached too, so
a second window of the same job reuses the download. The recovery still
reports the job and its `job_api_log` source through `source` and
`source_jobs`. A refused or failed status request falls through to the plain
read, so the provider's own error is returned and never a cached log. A
failure to read or write the cache is a miss, never a read error.

- **Never cached.** A job or run that is not completed, an empty log, a log
over the 8 MiB source read limit, a log with a secret found only across
lines, a read whose status request failed, and a status answer that names no
host and repository. A failed status request or cache file falls back to the
plain read; the cache never turns a read into an error.

- **Bounds.** At most 256 MiB in total and 24 hours per entry. Every store
removes entries past either bound (oldest first for size, and staging files a
crashed write left behind), and a lookup removes the entry it finds expired.

- **Access boundary.** Broker-only state: every agent sandbox masks the
directory, so a confined worker reads a cached log only through the tool's
bounded, redacted response.

- **Inspection.** The cache has no command and its responses carry no hit
flag. List `<Orbit data root>/state/github-run-logs/` to see cached entries;
the sibling lock, `<Orbit data root>/state/github-run-logs.lock`, sits beside
that directory under `<Orbit data root>/state/`. Read `source` and
`source_jobs` on a `github.run.logs` response to tell a run-scoped read from a
job-log recovery; use `github.auth.status` for quota state, which is not cache
state. Do not expect a hit indicator the tool does not report.

- **No settings.** There is nothing to configure. Do not invent keys for it.

## Search indexes

```bash
orbit doctor                              # search-index coverage
orbit search reindex                       # rebuild task chunks
```

Indexing is idempotent and safe to re-run. Reindex after bulk task imports or
a restore. → [search.md](../../orbit/references/search.md)

## What is evidence and must not be edited

Files under `.orbit/state/job-runs/`, `.orbit/state/audit/`, and
`~/.orbit/state/` are the record of what happened. Never edit them to make a
warning disappear or a run look successful. If a run is wrong, fix the cause and
re-run; the failed run stays as history.

For a validated task snapshot and same-authority recovery, use
[publication-bind.md](publication-bind.md). It does not back up logs, credentials, or
scheduler state.

Task bundles can also be moved deliberately — `orbit task export` and
`orbit task import` handle portable archives, and `orbit task reindex` rebuilds
the registry index from on-disk bundles after a restore. Never delete
`tasks/index.sqlite` to fix drift: it holds the allocator's issued-id high-water
mark for every workspace.

When it is lost, stop Orbit processes, reindex every
workspace, and set the mark with `orbit workspace init --force --task-id-start <N>`
above every id ever issued; Orbit refuses new task ids until then.

For archive migration, `orbit task export --output <archive.tar.zst> --ids <id>,<id>`
selects tasks (omitting IDs exports all). `orbit task import <archive.tar.zst>`
defaults to renumbering collisions and rewriting references; choose
`--on-conflict fail` or `skip` deliberately when appropriate. Inspect the returned
ID mapping.

To keep a read-only mirror of another host's tasks in step, use
`--on-conflict owner-wins`: it replaces only bundles whose task-ID prefix belongs
to that host, never touches locally minted IDs, and is safe to re-run. Import is a different operation from publication's strict
same-authority, identical-retry recovery; do not substitute it to bypass a
publication ownership or divergence error.

---
type: runbook
summary: Produce hard-timing evidence for a perf task — frozen store fixture, exact release build pair, serial base/candidate/control runs — and attach it for review.
tags: [performance, development, validation]
paths: ["scripts/perf/**"]
related_features: []
related_artifacts: [ORB-26322, ORB-26395, ORB-25652, ORB-25672, ORB-25638]
last_validated: 2026-11-21
---

# Measure a Perf Task With the Paired Timing Protocol

Use this when a task's acceptance criteria gate on a hard timing number (a p95 bound for a
CLI command or an HTTP route). It is the default evidence route the task authoring guide
and task-pilot's hard-timing check accept, so a perf brief cites this runbook instead of
inventing a fixture, build and sampling procedure. Informational timings do not need it.

The protocol has four scripts under `scripts/perf/`:

| Script | Does |
|---|---|
| `fixture.py snapshot` | Freezes one workspace of a store into a fixture, reading the source read-only. |
| `fixture.py restore` / `clean` | Makes / removes a disposable root (own `HOME` and workspace) from that fixture. |
| `build-pair.sh` | Builds the release `orbit` binary for the baseline and the candidate revision, or for an uncommitted working tree. |
| `paired.py` | Writes a bounded snapshot-manifest summary, checks output parity, then times base, candidate and an unchanged-build control. |

## Owner, host and safety

- **Host.** The run happens on the ws_orbit owner host (currently dk-server-2, which
  measured ORB-25652 and ORB-25672), so timings compare against the same machine class.
- **Owner.** The implementing leaf of the perf task owns its serial run. It holds the
  perf lane (below) while it builds and samples, and touches only restored fixture roots
  and the `orbit web serve` processes `paired.py` starts on them. It never touches the live
  store, `orbit-web.service`, the scheduler or a drain.
- **Live load.** A live drain shares the host. The control arm absorbs that: a noisy host
  widens the control spread and yields `inconclusive`, never a false `pass`.
- **Read-only source.** `fixture.py snapshot` opens every source SQLite file through a
  `mode=ro` URI connection (`open_source_read_only` in `scripts/perf/fixture.py`) and
  copies other files by reading them. It never writes to, write-locks or reconciles the
  store, and never starts or signals a service.
- **No credentials.** The snapshot copies no credential or env file: `.zprofile` and other
  shell profiles, `clock.env`, `*.env`, token, secret, credential and key files, and
  `plugin-secrets/`, `ssh/` and `.gnupg/` trees are skipped wherever they sit, and a copied
  `.git/config` loses any userinfo in its remote URLs. The manifest counts what was skipped.
- **Protected roots.** `restore` and `clean` refuse a target that is, resolves (through
  symlinks) under, or contains the live global root (`~/.orbit`) or any checkout `.orbit/`
  the live registry names. A run worktree's own scratch, `.orbit/state/worktrees/<run>/.orbit/tmp/`,
  is allowed. `clean` also refuses a directory that `restore` did not create.
- **No network.** CLI-mode samples run under `unshare --map-current-user --net` when the
  host allows unprivileged namespaces, and `paired-summary.json` records whether it did.
  HTTP-mode servers stay on host loopback so the harness can reach them. Nothing in the
  protocol starts an agent, so no provider is contacted.

### Serial lane

`build-pair.sh` and `paired.py` hold one host-wide `flock` lane for their whole run, so two
perf tasks never build or sample at once. The lock is
`$HOME/.orbit/cache/perf-lane/lane.lock` of the user's passwd home. It sits beside the
build budget's locks, outside every store and state directory, and is writable from
Orbit's Linux activity sandbox (`/var/tmp` is read-only there). `ORBIT_PERF_LANE_LOCK`
overrides it; every perf run on a host must use the same path. While held, the file names
its holder:

```json
{"entry": "paired.py", "host": "<host>", "pid": 58353, "run_id": "jrun-…", "started_at": "2026-11-21T09:54:42Z", "task_id": "ORB-…"}
```

A run that cannot take the lane within `--lane-wait` seconds (default 900, or
`ORBIT_PERF_LANE_WAIT_SECONDS`) exits 2 with `perf lane busy: <holder>`.

### Disk gate and cleanup

Before creating a snapshot, restore root, builds dir or results dir, each script reads
`df --output=pcent <parent>`. At 80% or more it creates nothing and exits 2 with the
reading, citing RB-11 (ws_constellation runbooks). Each script removes the scratch it made
when it exits (exported sources and target dirs, per-round roots) and removes its output
directory too when it fails. A refused parity check keeps `parity.json` for diagnosis.
`--keep` keeps everything for debugging.

## Choose the baseline and candidate

- **Baseline** is the merge-base the task branch starts from, by commit id. In a managed
  run this is the delivery's `base_sha`; otherwise `git merge-base <task-branch> agent-main`.
- **Candidate during `implement_bundle`** is the uncommitted working tree. HEAD is still
  the pinned base, so pass `--candidate-worktree <checkout>` instead of a candidate
  commit. The script snapshots tracked and untracked files, excluding ignored ones,
  into a temporary commit (`GIT_INDEX_FILE`, `ls-files --stage` into `update-index --index-info`,
  `git add --all`, `write-tree`, `commit-tree -p HEAD`). The snapshot's objects and index live in a
  throwaway directory, so the checkout's HEAD, index, files and git metadata stay
  untouched, including when that git dir is read-only. `provenance.json` names that
  commit and tree and sets `revisions.candidate.working_tree_snapshot` to true.
  A worktree with no such changes resolves to HEAD, and the equal-commit refusal
  fires when the base is that same commit.
- **Candidate after the delivery commit** (a review rerun) is the reviewed final head,
  passed as `--candidate <commit>`.
- Both build with the same command, profile and toolchain on the same host;
  `provenance.json` proves it. `build-pair.sh` refuses the same commit twice, and refuses
  two revisions that produce byte-identical binaries (ORB-25652's first candidate silently
  reused the baseline binary). `paired.py` times the binaries this script writes; it does
  not need a separate candidate commit when the build was a working-tree snapshot.
- The fixture's store schema must not be newer than the baseline binary understands.
  Snapshot a store the baseline can open; each arm migrates its own fresh restored copy.

## Procedure

Placeholders: `<slug>` names the measurement (for example `doctor-p95`); `<ws>` is the
workspace to freeze (registry id, name or storage id); `<scratch>` is the run's
`$ORBIT_SCRATCH_DIR` (`.orbit/tmp/` in a managed worktree). Run every step from the
repository root of the checkout that holds these scripts.

1. Freeze the fixture. Add `--git-metadata <checkout>` when the measured code reads Git
   state (doctor does). `--global-root <dir>` selects a different source global root
   (default `~/.orbit`); `--exclude <path>` leaves a workspace `.orbit`-relative path out.

   ```sh
   python3 -I scripts/perf/fixture.py snapshot --workspace <ws> \
     --out <scratch>/perf-<slug>/snapshot --git-metadata <checkout>
   ```

   The fixture keeps the workspace's task bundles with history, comments and artifacts,
   its task-index rows, its retained job runs, run states, steps and audit events, its
   routine fires, cursors and pauses, and its `.orbit/` state directories
   (`state/worktrees`, `state/logs` and `tmp` excluded). Rows of other workspaces are
   dropped: in the global store a row stays when its `workspace_id` or `source_workspace`
   names this workspace or is NULL, and tables without either column are kept whole.
   ORB-25652's first fixture left out retained runs and routines, so its probes read 1–33 ms
   against 300+ ms deployed; check `state_distributions` in the manifest before you rely on
   a snapshot. For a linked reftable checkout, the snapshot retains the shared ref
   stack and reconstructs the selected HEAD and per-worktree refs in the standalone
   copy; selected per-worktree reflog history is not retained.

2. Build the pair (two cold release builds; about 15 minutes each on the owner host).
   During `implement_bundle`, build the uncommitted candidate from the checkout:

   ```sh
   scripts/perf/build-pair.sh --base <base-commit> --candidate-worktree <checkout> \
     --out <scratch>/perf-<slug>/builds
   ```

   After the delivery commit, a review rerun can name that head instead:

   ```sh
   scripts/perf/build-pair.sh --base <base-commit> --candidate <candidate-commit> \
     --out <scratch>/perf-<slug>/builds
   ```

3. Run parity and timing in one call. Parity runs first, once per arm, and the timing
   starts only if every arm matches:

   ```sh
   # CLI surface
   python3 -I scripts/perf/paired.py --builds <scratch>/perf-<slug>/builds \
     --snapshot <scratch>/perf-<slug>/snapshot --out <scratch>/perf-<slug>/results \
     --cli 'doctor --json' --parity 'doctor --json' --parity-mask duration_ms \
     --rounds 3 --samples 10 --warmup 1 --gap-ms 250 --bound-ms <n> \
     --qualifying-task-count '<check>=<observed-count>'

   # HTTP surface
   python3 -I scripts/perf/paired.py --builds <scratch>/perf-<slug>/builds \
     --snapshot <scratch>/perf-<slug>/snapshot --out <scratch>/perf-<slug>/results \
     --http '/api/residents?all=true' --parity 'doctor --json' --parity-mask duration_ms \
     --rounds 3 --samples 10 --bound-ms <n> \
     --qualifying-task-count '<check>=<observed-count>'
   ```

   Repeat `--qualifying-task-count NAME=COUNT` for every qualifying-task count the
   acceptance criteria ask you to check. Record the observed count, not the threshold;
   the flag records evidence and does not change or apply any measurement check.

   What happens:

   - **Arms.** `base`, `candidate`, and `control`: the base binary copied to a second
     path, so the spread of an unchanged build is measured rather than assumed.
   - **Order.** Rounds interleave forward then reversed (`base candidate control`, then
     `control candidate base`, …): with two arms over three rounds this is ORB-25652's
     A B B A A B. The order is recorded.
   - **Rounds.** Every round restores a fresh root from the snapshot, discards
     `--warmup` runs, then takes `--samples` sequential samples at least `--gap-ms` apart,
     recording wall ms and exit status. A nonzero exit (or a non-2xx HTTP status) fails
     the run.
   - **CLI mode** times `<binary> <orbit args>` in the restored root.
   - **Bounds.** Every parity and CLI sample command runs as the leader of its own
     process group, with its stdout drained while it runs. One that runs past
     `--command-timeout` seconds (default 300) or writes more than
     `--output-limit-bytes` of stdout (default 16 MiB) gets SIGTERM, then SIGKILL after a
     5 s grace, across its whole group, and the run exits 2 naming the arm and the bound.
     A command that exits cleanly still has its group swept, so no descendant outlives
     it. Both bounds are recorded under `limits` in `parity.json` and in the
     `protocol` of `paired-summary.json`.
   - **Cancellation.** SIGTERM, SIGHUP or Ctrl-C stops the running arm command or
     `web serve` group, releases the lane, cleans up as a failed run does and exits
     128 + the signal number. SIGKILL of the harness skips that cleanup; the enclosing
     run's containment reclaims what it leaves.
   - **HTTP mode** starts each arm's own `orbit web serve --no-open --port 0` (an
     ephemeral port) on its restored root, waits for its address and a first answer, times
     paced GETs of the path, then stops and reaps the server. Parity and timed GETs
     do not follow redirects: the requested path's status and body are the response,
     and a redirect fails the non-2xx check.
   - **Parity.** `--parity` runs once per arm before sampling; in HTTP mode each arm's
     response body for the timed path is compared too. A match requires a successful
     observation from every arm. When `--parity` is set, that command must exit 0 on
     every arm. When `--http` is set, that probe must return a 2xx status. At least one
     of those observations has to be present, so omitting `--parity` in CLI mode is a
     missing observation, not an agreement. An explicit `--parity` with no command words
     (an empty value, or only spaces) is a usage error, so it cannot stand in for an
     omitted one. Identical non-zero exits are a failed probe,
     not a match, even when stdout is empty on every arm. JSON outputs are compared after
     the `--parity-mask` fields (key names at any depth, or dotted paths) are removed and
     the root path is replaced by `<root>`. A missing observation, a failed probe or a
     difference writes
     `parity.json` with `match` false and exits 2 without sampling, so no bounded timing
     verdict is issued. Mask only timing fields; a mask that hides a behaviour change
     defeats the check.
   - **Host pressure.** `/proc/loadavg` and the number of Orbit-managed processes (those
     carrying `ORBIT_MANAGED_RUN_CONTEXT`) are read before and after each round.
   - **Aggregation.** Per round, the nearest-rank p95 (ORB-25638): the ⌈0.95·n⌉-th
     smallest sample. Per arm, the median of its round p95s. The control spread is the
     larger of the range of the control's round p95s and |median(control) − median(base)|.
   - **Verdict** (with `--bound-ms`): `pass` only when the candidate median ≤ bound −
     control spread; `fail` when the candidate median is over the bound; `inconclusive`
     otherwise, which means rerun with more `--rounds`. Without a bound the verdict is
     `informational`. The exit status is 0 for any verdict; refusals exit 2.

4. Attach the evidence under `performance/<slug>/` and cite the paths in the
   execution summary. `paired.py` writes `results/snapshot-manifest-summary.json`, a
   summary no larger than the 1,048,576-byte artifact limit. It records the full manifest's
   host path, byte size, SHA-256, file count, supplied qualifying-task counts, snapshot id
   and base/candidate revision ids. The full `snapshot/snapshot-manifest.json` remains on
   the host for restore and is not attached; its digest in the summary identifies it.
   Split each of the five attachable files into numbered parts when needed so every
   attachment fits, including large raw sample files. Use a fresh evidence directory so
   old parts cannot be mixed into the handoff.

   ```sh
   set -e
   cd <scratch>/perf-<slug>
   mkdir evidence
   for f in results/snapshot-manifest-summary.json builds/provenance.json results/parity.json \
            results/raw.json results/paired-summary.json; do
     name=$(basename "$f")
     (cd "$(dirname "$f")" && sha256sum "$name") > "evidence/$name.sha256"
     split -b 1048576 -d -a 6 "$f" "evidence/$name.part-"
   done
   test "$(sha256sum snapshot/snapshot-manifest.json | cut -d' ' -f1)" = \
     "$(jq -r '.manifest.sha256' results/snapshot-manifest-summary.json)"
   for f in evidence/*; do
     orbit tool run orbit.task.artifact --input "{\"action\":\"put\",\"id\":\"<task-id>\",
       \"source_path\":\"<scratch>/perf-<slug>/$f\",\"path\":\"performance/<slug>/$(basename "$f")\"}"
   done
   ```

   To verify the handoff, download all parts and checksums into an empty directory,
   then reconstruct and check each file as below (shown for the summary). Its digest
   must also match the host-side manifest before the fixture is cleaned up.

   ```sh
   cat snapshot-manifest-summary.json.part-* > snapshot-manifest-summary.json
   sha256sum -c snapshot-manifest-summary.json.sha256
   ```

5. Clean up. The snapshot and builds dirs are plain directories the run created; remove
   them once attached (a root left by `fixture.py restore` is removed with
   `fixture.py clean --into <dir>`):

   ```sh
   rm -rf <scratch>/perf-<slug>
   ```

### Use a restored root by hand

`fixture.py restore --snapshot <snapshot-dir> --into <dir>` verifies every file's size and
SHA-256 against the manifest (and refuses unlisted files) before it copies anything. It
lays out `<dir>/home` (the fixture's global root is `<dir>/home/.orbit`) and
`<dir>/checkout` (workspace root `<dir>/checkout/.orbit`, plus `.git` when the snapshot has
Git metadata), rewrites the source's absolute paths in the copied registry, configs,
generation and layout records, task records and databases to the new location, leaves
verified task-artifact blobs under `artifacts/files/` byte-for-byte as snapshotted (a
read checks each blob's size and SHA-256), and prints the env:

```sh
export HOME="<dir>/home"
export USERPROFILE="<dir>/home"
export XDG_CONFIG_HOME="<dir>/home/.config"
export GIT_CEILING_DIRECTORIES="<dir>"
cd "<dir>/checkout"
# workspace selector: --workspace <name>  (registry id <id>)
```

Unset every inherited `ORBIT_*` and `GIT_*` variable first, as `paired.py` does. The
restored env deliberately sets no `ORBIT_ROOT`: an explicit `ORBIT_ROOT` (or `--root`)
pins one directory as both the workspace and the global root (`resolve_orbit_roots` in
`crates/orbit-runtime/src/resolve.rs`), so the fixture's global store and task bundles
would not be found. The workspace resolves instead from the initialized `.orbit/` in the
working directory (resolution step 3, ahead of linked-worktree discovery), and the global
root from `HOME`; `restore` refuses a root whose `.orbit/config.yaml` does not name the
fixture workspace, so resolution cannot fall through to an enclosing checkout.

## Artifact schemas

The five attached protocol artifacts are JSON objects with `"schemaVersion": 1` and a
`kind`. The full snapshot manifest stays on the host and is not one of the attachments.

**`snapshot-manifest-summary.json`** (`kind: orbit-perf-snapshot-manifest-summary`):

| Field | Meaning |
|---|---|
| `manifest.path_on_host`, `size_bytes`, `sha256`, `file_count` | Host path and integrity summary for the full manifest, which remains in the snapshot directory |
| `qualifying_task_counts` | Observed counts supplied with repeated `--qualifying-task-count NAME=COUNT` arguments |
| `snapshot.id`, `epoch`, `source_workspace` | Snapshot identifier derived from workspace id, snapshot epoch and the first 16 manifest-digest characters, plus its source and epoch |
| `revisions.base`, `candidate` | Full commit ids from `provenance.json` |

The paired harness refuses to write this summary if it exceeds 1,048,576 bytes.

**`snapshot-manifest.json`** (`kind: orbit-perf-snapshot`):

This full manifest stays on the host for restore and local verification; attach its
bounded summary instead.

| Field | Meaning |
|---|---|
| `source` | `selector`, `registry_id`, `name`, `storage_id`, `global_root`, `home`, `repo_root`, `orbit_dir` of the source |
| `source_access` | Statement that every SQLite source was opened `mode=ro` and nothing wrote to or signalled it |
| `snapshot_epoch`, `snapshot_at` | When the snapshot was taken (Unix seconds, UTC) |
| `workspace_keys` | Workspace identifiers rows were kept for |
| `databases` | Per copied database: `method` (`filtered-copy-single-read-transaction` or `online-backup-api`), `filtered`, `scope_columns`, `kept_whole`, and `tables` (row count per table) |
| `state_distributions` | `job_runs_by_state`, `job_run_steps_by_state`, `routine_fires_by_state`, `routine_fires_by_routine`, `routine_cursors`, `routine_pauses` |
| `git_metadata` | `null`, or the checkout, per-worktree Git dir and Git common dir copied |
| `excluded` | Counts of credential-like and non-regular files skipped, and the excluded workspace paths |
| `files` | Every copied file: `path`, `size`, `sha256` |

The global store and task index are copied table by table inside one read transaction of a
`mode=ro` connection, because the online backup API copies every page (17 GB for the shared
store) and its append-only triggers refuse the deletes a backup would then need. Workspace
databases are copied whole with the online backup API.

**`provenance.json`** (`kind: orbit-perf-build-pair`): `revisions.base` and
`revisions.candidate` (each `commit`, `tree`, `working_tree_snapshot`, `binary`,
`binary_sha256`, `cargo_config_sha256`, `cargo_lock_sha256`), `requested` (`base`,
`candidate`, `candidate_worktree`), `profile`, `features`, `cargo_command`,
`build_stub`, `source_export`, `cargo_target_dir`, `rustc_vv`, `cargo_version`,
`codegen_env` (RUSTFLAGS, wrapper, target, C toolchain and `CARGO_PROFILE_RELEASE_*`
values; `null` when unset), `rustc_wrapper`, `host`, `machine_id`, `created_at`.
`working_tree_snapshot` is true only on a candidate built from `--candidate-worktree`
whose tree differed from HEAD; `commit` and `tree` are that snapshot. The snapshot
commit is not stored in the checkout. `build_stub` is true only for the
`ORBIT_PERF_BUILD_PAIR_STUB` test hook and is never a measurement.

**`parity.json`** (`kind: orbit-perf-parity`): `command`, `http_path`, `masks`, `match`,
`differing_arms`, `observation_failures` (arm to why that arm has no successful
observation; empty when every arm succeeded), and `arms.<arm>`. The top-level `command` is
the `--parity` string, or `null` when no parity command was given. An arm record holds
`command` (`exit`, `output`) only when `--parity` was supplied, and `http` (`status` and
`body`, or `error` when the request failed) only in HTTP mode. So a CLI run without
`--parity` leaves every arm record empty, which is a missing observation; an HTTP-only run
without `--parity` has `command: null` and arm records holding `http` alone. Each output is
`{format: json|text, value}` after masking. `match` is true only when `observation_failures`
and `differing_arms` are both empty.

**`raw.json`** (`kind: orbit-perf-raw`): `surface` and `samples`, one entry per sample
including warmups: `arm`, `round`, `index`, `warmup`, `ms`, `exit`.

**`paired-summary.json`** (`kind: orbit-perf-paired-summary`): `surface`, `protocol`
(rounds, samples, warmup, gap_ms, aggregation), `round_order`, `revisions`,
`binary_sha256` per arm, `snapshot` (path, epoch, source workspace,
manifest-summary path), `round_p95_ms` and
`median_p95_ms` per arm, `control_spread_ms` (`value`, `control_range`,
`control_vs_base`), `candidate_minus_base_ms`, `bound_ms`, `verdict`, `reason`, `network`
(`isolated` and `method` or `reason`), `host_pressure` (per round `before`/`after`
`loadavg` and `orbit_leaf_processes`), `lane` (the holder record), `host`, `machine_id`.

## Measurement protocol paragraph for a perf brief

Paste this into the brief and fill the five slots:

> **Measurement protocol.** Evidence follows `docs/runbooks/perf-timing-protocol.md` on the
> ws_orbit owner host; the implementing leaf owns the serial run and holds the perf lane.
> Fixture: `scripts/perf/fixture.py snapshot --workspace ws_orbit --git-metadata <checkout>
> --out <scratch>/perf-<slug>/snapshot` taken at the start of the run, restored fresh for
> every round. Builds during `implement_bundle` (HEAD is still the pinned base; the
> candidate is uncommitted):
> `scripts/perf/build-pair.sh --base <merge-base commit> --candidate-worktree <checkout>
> --out <scratch>/perf-<slug>/builds`.
> That snapshots the working tree without moving HEAD, the index or files, and
> provenance marks the snapshot commit and tree. A rerun after the delivery commit
> uses `--candidate <reviewed final head>` in place of `--candidate-worktree <checkout>`,
> with the same `--base` and `--out`.
> Measurement: `scripts/perf/paired.py --builds <scratch>/perf-<slug>/builds
> --snapshot <scratch>/perf-<slug>/snapshot --out <scratch>/perf-<slug>/results
> --<SURFACE: cli|http> '<COMMAND OR PATH>' --parity
> '<PARITY COMMAND>' --parity-mask <MASK FIELDS> --rounds 3 --samples 10 --warmup 1 --gap-ms 250
> --bound-ms <BOUND> --qualifying-task-count <CHECK>=<OBSERVED-COUNT>`: base, candidate and unchanged-build control arms in balanced
> interleaved rounds, per-round nearest-rank p95, per-arm median of round p95s, control
> spread = max(control range, |control − base|). The criterion passes only on verdict
> `pass` (candidate median ≤ bound − control spread); `inconclusive` means rerun with more
> rounds, never pass. Repeat the qualifying-task-count flag for each observed count in the
> criterion. Attach the parts and checksums for `snapshot-manifest-summary.json`,
> `provenance.json`, `parity.json`, `raw.json` and `paired-summary.json` under
> `performance/<SLUG>/` and cite those paths in
> the execution summary.

## Replacement Filing text for the `perf-review` auto-task

The ws_orbit `perf-review` auto-task template's `## Filing` section should read as below.
Applying it to the live definition is an operator step after this runbook lands.

```markdown
## Filing

One task per root cause, not per symptom (one slow query behind three endpoints is
one task). Each filed task: `type: bug` for a regression, `feature` for a hot spot;
`tags: ["performance", "perf-review"]`; `complexity` medium, or hard when the fix
crosses crates or changes a store schema or index; `context_files` with verified
`file:` selectors for the code you traced the cost to. The description carries the
measured numbers (before, and the previous pass for a regression), the reproduction
command, the suspected cause with code pointers, and a concrete target. Acceptance
criteria are observable and no behaviour changes.

For a hard timing criterion, use the paired timing protocol in
`docs/runbooks/perf-timing-protocol.md` as the evidence route: paste its "Measurement
protocol" paragraph into the description and fill its slots (surface `cli` or `http`,
the command or GET path, the parity command and mask fields, the bound in ms, and the
artifact slug). Set the bound so the target holds with margin over the unchanged-build
control spread this pass measured; when no stable bound is known yet, make the timing
informational and gate on a deterministic proxy (query, request or payload counts)
instead. The implementing leaf runs the protocol on the ws_orbit owner host against the
candidate and baseline it builds itself; never require numbers from the deployed
service, a live drain, or after a deploy; the operator confirms those after deploy.
Tasks land in `proposed`. Do not approve them.
```

## Refusals

| Exit 2 message | Cause | Fix |
|---|---|---|
| `refusing to restore into … live Orbit root …` | Target is, resolves under or contains `~/.orbit` or a registered `.orbit/` | Restore under the run's scratch dir |
| `… failed digest verification, refusing to restore: …` | A snapshot file was changed, removed or added after the snapshot | Take a fresh snapshot |
| `workspace … has no initialized identity: …` (snapshot) | The registered checkout's `.orbit/config.yaml` is missing or names no `workspace_id` | Initialize that checkout with `orbit workspace init --force`, then snapshot again |
| `snapshot … records no workspace identity …` (restore) | The snapshot was taken from an identity-less source | Take a fresh snapshot of an initialized workspace |
| `… built identical binaries …` | The two revisions differ only in metadata | Pick revisions whose content differs |
| `base and candidate resolve to the same commit …` | Same commit given twice, or `--candidate-worktree` on a checkout with no uncommitted changes (ignored files do not count) and a base that is that HEAD | Check the revision selectors, or make the working-tree change first |
| `--candidate-worktree must be the checkout root …` | `--candidate-worktree` names a subdirectory of the checkout | Pass the root path the message names (`git rev-parse --show-toplevel`) |
| `parity mismatch between arms …` | An arm has no successful observation (missing `--parity` in CLI mode, or a probe that did not exit 0 / return 2xx), or outputs differ after masking | Read `parity.json` (`observation_failures` and `differing_arms`); fix the probe or the behaviour change, or mask only a timing field |
| `paired.py: error: --parity takes an orbit command …` | `--parity` given with no command words (an empty variable, say) | Pass the command, or leave `--parity` out |
| `<arm> parity command timed out after <n> s …` / `<arm> round <r> sample <i> timed out …` | An arm command stalled past `--command-timeout` | Find why it hangs on the fixture, or raise `--command-timeout` for a genuinely slow surface |
| `<arm> … wrote more than <n> bytes of stdout …` | An arm command's output passed `--output-limit-bytes` | Pick a smaller probe, or raise `--output-limit-bytes` |
| `perf lane busy: <holder>` | Another perf run holds the lane | Wait for it, or raise `--lane-wait` |
| `disk gate: … at or above 80% …` | The parent filesystem is too full | Free space per RB-11, then rerun |

## Related references

- [Task authoring guide: timing criteria](../../crates/orbit-core/assets/skills/orbit/references/task-authoring.md)
- [Build budget](./build-budget.md) — the admission `build-pair.sh` builds under.
- [Soak-test dashboard memory](./web-memory-soak.md) — a synthetic-fixture harness for memory, not timing.

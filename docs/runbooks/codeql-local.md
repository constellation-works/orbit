---
type: runbook
summary: Run local Rust CodeQL once per delivery for code-scanning repairs, security-tagged tasks and CodeQL setup changes, with a verified host toolchain cache, bounded concurrency, threaded evaluation and exact-input reuse, and reject incomplete semantic extraction.
tags: [rust, codeql, security, validation]
paths: ["scripts/codeql-rust-local.sh", ".github/codeql/**"]
last_validated: 2026-10-18
---

# Run local Rust CodeQL for code-scanning repairs and security changes

Hosted CodeQL analyses `agent-main` pushes (not pull requests). Use this
procedure before handoff only when the agent guide (`CLAUDE.md`, "Local Rust
CodeQL checks") requires it: a code-scanning or CodeQL repair, a task tagged
`security` or `security-review`, or a change to `.github/codeql/**` or
`scripts/codeql-rust-local.sh`. For a code-scanning repair, check the
identified rule at every affected location; in the other cases, run a relevant
Rust query or suite. An extractor can exit zero after losing semantic analysis;
a clean query against that database is invalid evidence.

## Prerequisites

Use an existing CodeQL bundle (CLI plus Rust query packs), `rustup`, Cargo
proxies on `PATH`, and a working Bubblewrap installation that can create its
Linux namespaces. The owner-side fulfilment runs candidate-controlled scripts
inside Bubblewrap, with the detached evidence checkout as the only writable
task-controlled path. The script does not install CodeQL or global toolchains.
Rust downloads and any Cargo dependency fetches use only the executor's granted
network access. Start in the checkout being validated.

When validating a hosted alert, use the same CodeQL bundle version as the
hosted analysis when possible. Record the local version from `codeql version`
and the hosted analysis version shown with the alert or its analysis metadata.
If they differ, record both versions in the evidence and treat the local result
as limited evidence for that hosted alert.

The default extractor toolchain is **Rust 1.97.0 plus rust-src**, confirmed for
CodeQL **2.27.1**. This is separate from the repository's build toolchain. For
another bundle, inspect its extractor's requested version and pass
`--toolchain <version>`; do not substitute the host's installed stable version.

Run it on Linux, the platform of the hosted CodeQL job. Other hosts cannot
produce a complete run; see [Non-Linux hosts](#non-linux-hosts).

## Procedure

Run a named query or suite appropriate to the Rust change. This example runs
the bundled Rust security suite; a targeted query may replace the selector
with a `.ql` path or pack selector.

```bash
codeql version
scripts/codeql-rust-local.sh --ram 16384 \
  codeql/rust-queries:codeql-suites/rust-security-extended.qls
```

`--ram` is in MiB and applies to extraction and analysis. The requested default
is 16384. On Linux, the effective value is
`min(requested, floor(MemTotal * 0.6 / slots))`, with `/proc/meminfo`'s
`MemTotal` converted from KiB to MiB and the [resolved slot count](#concurrent-runs).
This reserves 40% of host memory for the OS and other work. The script prints a
note when it clamps, and refuses before extraction if the effective value is
below the minimum **2048 MiB**. Reduce the slot count or use a host with more
memory in that case. Unreadable or invalid meminfo keeps the requested value;
non-Linux hosts retain their exit-3 refusal without reading meminfo.
A previous targeted query exhausted the heap at 4096, so the minimum is only
an admission floor, not a guarantee that every query fits. Choose a requested
value appropriate to the query and executor; the clamp cannot account for
other processes' actual memory use. The behavior harness overrides the meminfo
path with `_ORBIT_CODEQL_MEMINFO_PATH` to exercise budgets deterministically.

Run it once per delivery, after the last Rust change, against the final
candidate. A run takes minutes, plus any wait for a [slot](#concurrent-runs),
so run it in the foreground with a timeout of at least 45 minutes: an
interrupted run leaves no SARIF and must start over. Read the `results.sarif`
of that run rather than running again. Run again only when Rust sources, the
CodeQL configuration or model extensions change, the run failed, or the review
needs another query. An identical invocation [reuses](#reusing-a-passing-run)
the passing run instead of repeating it.

Each run creates a fresh `codeql-rust-local.*` directory under
`$ORBIT_SCRATCH_DIR` (or `<checkout>/.orbit/tmp` when unset). It isolates
`CARGO_HOME`, Cargo build output, temporary files, and CodeQL caches there.
The run directory is printed before preparation. It contains:

- `toolchain.log`: preparation of the pinned toolchain and rust-src, or the
  line recording that the verified host cache was used and no rustup install
  ran.
- `codeql-config.yml`: the effective configuration used for extraction.
- `extraction.log`, `extraction-problems.log`, `codeql-logs/`, and
  `database/log/`: extraction evidence, including extractor-only warnings.
  `database/src.zip` holds every extracted source file.
- `analysis.log` and `results.sarif`: query execution and results.
- `inputs.sha256`: written by a passing run whose inputs did not change while
  it ran; see [Reusing a passing run](#reusing-a-passing-run).

A run that fails keeps everything, including the database dataset
(`database/db-rust/`), `cargo/`, `target/`, `tmp/` and CodeQL caches, for
diagnosis. A passing run removes those after the SARIF is written and keeps
only the evidence listed above, about 40 MB instead of 1.5 GB for this
repository.

### Host toolchain cache

The pinned toolchain is installed once per host, not once per run, into
`$ORBIT_CODEQL_CACHE_DIR/rust-<version>/rustup` (default
`~/.orbit/cache/codeql-rust-local`, the host cache that Linux implementer
sandboxes may write; see [compiler cache](compiler-cache.md)). The script
points `RUSTUP_HOME` at it; `CARGO_HOME`, `TMPDIR` and Cargo output stay per
run. Installation uses `--profile minimal --component rust-src
--no-self-update` and never writes to the user's `~/.rustup`. The minimal
profile is required: a default-profile install writes rust-docs, and
`macro.env.html` matches denyModify `**/*.env.*`.

Every run verifies the cached toolchain before using it: exactly one
toolchain, its `rustc --version` naming the pinned version, rust-src present,
and the SHA-256 digests of every file matching the `manifest.sha256` written
at install. A missing or failing install is reinstalled under an exclusive
lock into a staging directory, sealed read-only, and swapped in whole, so no
run sees a partial toolchain. Each run holds a shared lock on it until it
exits, so a reinstall never replaces a toolchain in use. The manifest detects
corruption, not tampering: it sits in the same writable cache, so the cache
carries the same trust as the compiler cache beside it.

When the cache cannot be created or written and holds no valid toolchain (a
reviewer profile without the cache grant, or [owner fulfilment](#owner-fulfilment)
on a cold host), the run says so and installs into its own `rustup/`, as
every run did before the cache. A valid cached toolchain is used read-only
in those sandboxes.

### Concurrent runs

Extraction and analysis saturate the disk long before the CPUs, so they run
under a host-wide counted lock: one of `ORBIT_CODEQL_SLOTS` slots, else the
number in the regular file `$ORBIT_CODEQL_CACHE_DIR/slot-count`, else 2.
Set that file to an integer from 1 through 128; `ORBIT_CODEQL_SLOTS` takes
precedence. The locks stay in the distinct directory
`$ORBIT_CODEQL_CACHE_DIR/slots/`, as `slot-NNN.lock` files. The count file
works before the first run and with an already populated cache. For example:

```bash
codeql_cache="${ORBIT_CODEQL_CACHE_DIR:-$HOME/.orbit/cache/codeql-rust-local}"
mkdir -p "$codeql_cache"
printf '1\n' > "$codeql_cache/slot-count"
```

Older instructions used a regular `slots` file for the count, colliding with
the lock directory. The script migrates that regular file to `slot-count`
under an exclusive lock before admission. It preserves the count and leaves
an existing `slots/` directory and its lock files untouched, so runs using
the older lock paths still exclude new runs. If migration fails, or both
count files exist, the script stops before extraction; reconcile the files
or make the cache writable before retrying.

Every run reports
`acquired CodeQL slot N of M after queueing X.Ys`; a run that has to wait
first prints `waiting for a CodeQL slot` and repeats a progress line every
45 seconds. Toolchain verification and installation are outside the slot. If
the slot locks cannot be opened, the run warns and proceeds without one.

### Threads and run time

CodeQL imports and evaluates on one thread unless told otherwise. The script
passes `--threads` to extraction (TRAP import, and a hint to the extractor)
and to analysis: `ORBIT_CODEQL_THREADS` when set (1 through 1024), else the
processors available divided by the slot count, so full slots share the host
without oversubscribing it. The thread count does not change results. Each
run reports `extraction took Xs` and `query analysis took Ys` with the thread
count.

On the 32-processor Linux box with two slots, nine completed single-threaded
runs of `rust-security-extended` spent 3.5–9.8 minutes extracting (the
extractor itself 2.7–6.7, then TRAP import 0.8–3.0) and 11.2–15.8 minutes
evaluating. Query compilation took 2–5 seconds, because the bundle ships the
suite's queries precompiled (`.qlx`), so a shared compiled-query cache would
save nothing and each run keeps its private one. With 16 threads, evaluation
took 6.0 minutes and TRAP import 1.2. The extractor's cost is loading the Cargo
workspace (fetching dependencies into the run's `CARGO_HOME` and building
build scripts and proc macros in its own target directory) and extracting
library and crate sources. Sharing those across runs would break the
per-run isolation above, so a run on this repository still takes about
12 minutes when the host is busy.

### Reusing a passing run

Before preparing anything, the script fingerprints the run's inputs:

- every file and symlink in the checkout except `.git/`, `.orbit/`,
  `target/`, the scratch directory and earlier run directories
- the effective configuration
- the query selector string
- the toolchain version and `codeql version`
- the `CODEQL_*`, `RUSTFLAGS`, `RUSTDOCFLAGS` and Cargo build, profile and
  target variables, other than those the script overrides

A selector that names an existing local path, including a `.ql` query or
`.qls` suite, is not reusable: the script cannot fingerprint its complete
dependency closure. A suite may reference queries outside the checkout, and a
query may import `.qll` libraries outside the selected path. Such selectors run
analysis every time, so edits to those dependencies cannot return stale SARIF.
The script writes no `inputs.sha256` for those runs. Use a pack selector when
you want reuse; for non-path selectors, the key includes the selector string,
CodeQL version and checkout inputs, but does not hash pack files CodeQL
resolves from an installation or package cache. Reuse is supported when that
pack content is fixed by the pinned CodeQL installation; pass `--fresh` if it
can change independently.

For a reusable selector, a passing run writes the digest to `inputs.sha256`
after rechecking it, so a checkout changed while the run was active is never
reused. When a run directory under the same scratch has a matching digest and
a `results.sarif`, the script prints that directory as its run directory, says
it is reusing it, and exits zero without extracting or querying. The evidence
there is the evidence of that invocation, and is as trustworthy as the scratch
that holds it, like any retained evidence; owner fulfilment always runs fresh.
`--fresh` runs again regardless, and the new passing run can itself be reused
when its selector is supported. A failed run is never reused. Any input change,
including an edit to a document, starts a fresh run.

## Source selection

CodeQL extracts every `.rs` file under the checkout that the configuration does
not exclude. The shared `.github/codeql/codeql-config.yml` excludes tests,
test-only helpers named `test_support.rs` (including those under `src/`),
examples and benches, and also Orbit state (`.orbit/`) and Cargo output
(`target/`). Neither exists in a hosted checkout, so hosted selection is
unchanged, but locally they hold worktrees, earlier runs' toolchains and
generated code.

The script writes the run's `codeql-config.yml` as the shared configuration
with run-specific entries added to the front of `paths-ignore`:

- The scratch directory, when it is inside the checkout, as the default
  `.orbit/tmp` is. This excludes the run's own rust-src and Cargo output as
  well as earlier runs and other residue in the same scratch.
- Every earlier run directory elsewhere in the checkout, left by a run whose
  scratch was a different directory inside the checkout. One is recognized by
  its `codeql-rust-local.XXXXXX` name (six letters or digits) and the
  `codeql-config.yml` each run writes first. The search skips `.git/` and the
  already excluded `.orbit/` and `target/`. A directory with that name but no
  configuration is not excluded.

Nothing needs to be cleaned first. The script refuses to start, before
preparing anything, if the scratch directory is the checkout, or if the
scratch directory or a recognized earlier run directory contains tracked files
or has characters that cannot be matched exactly, or if `paths-ignore` is not a
single top-level block list.

Excluded files are not repository inputs. For semantic analysis, the extractor
still loads, as libraries, the prepared rust-src, the dependency sources Cargo
fetches into the run's `CARGO_HOME`, and its own build-script output. CodeQL
uses the repository's `.github/codeql/extensions` model packs during analysis.
Other untracked files outside these exclusions are still extracted. To confirm
selection, rerun the `codeql resolve files` command recorded in
`database/log/database-index-files-*.log`. It lists the primary inputs, which
must be production sources only. In `src.zip`, every checkout path outside the
run directory must be one of those inputs. Keep the full listings locally
under the run directory for inspection. Task evidence should contain their
SHA-256 digests, entry counts, and bounded excerpts that identify the checked
production paths; never attach archives or full listings.

## Verification and failures

Exit zero means preparation, extraction-log checks, and query execution
completed, in this run or in the passing run it reused. **It does not mean
the rule is clean.** Inspect `results.sarif` for the findings relevant to the
selected query and change; for an identified
repair, inspect that rule and each affected location. Retain the full logs
and `src.zip` locally under `.orbit/tmp/`. Attach a compact JSON record
with the query selector, CodeQL version, validated source tree, extraction
checks, SARIF findings, and SHA-256 digests of the retained logs and source
archive. Attach only bounded log/listing excerpts (gzip when useful), each
within the artifact tool's 1 MiB limit; never attach `src.zip`, another archive,
or a full source listing. When practical, run the same query on the baseline to confirm it
detects the finding.
If that baseline run does not report the hosted alert, the local result is
non-confirming evidence and cannot confirm a repair. Record that no baseline
finding was reported along with the local and hosted CodeQL versions, and rely
on a hosted rescan to confirm the repair.
Source-side confirmation does not close hosted alerts; closure requires a
hosted rescan.

The script exits nonzero before querying if installation fails, extraction
fails, or console/detailed logs report unavailable semantic analysis, skipped
macro expansion, or any extraction warning/error. Diagnostics include the
prepared version and matching log lines, preserving the requested toolchain and
underlying rustup error when the extractor reports them. If a bundle needs a
different version, rerun with that pinned `--toolchain` value. If scratch is
unwritable or downloads are refused, record the denial as a validation blocker.
Do not treat the incomplete database as confirmation, install into global
rustup storage, or delete residue to make a run pass: a warning for a file
outside the intended sources is a source-selection defect. A source file
pulled into another module with `include!` is not a module of its own, so the
extractor skips its semantic analysis and the run refuses; declare it with
`mod` instead of excluding it. A failed query's partial SARIF is also unusable.

### Source-defined validation barriers

For a source-available validator, use `ReturnValue` in `barrierModel`, including
when it returns `Result<PathBuf, E>` or `Result<Option<PathBuf>, E>`:

```yaml
- ["orbit_crate::module::validated_path", "ReturnValue", "path-injection", "manual"]
```

On Linux with CodeQL 2.27.1 (rust-all 0.2.22), the real-CodeQL fixture below
reports `rust/path-injection` through
`ReturnValue.Field[core::result::Result::Ok(0)]` and through its nested
`.Field[core::option::Option::Some(0)]` form. The SARIF path traverses the
source-defined body, the returned `[Ok]` value and the caller's `TryExpr`.
Whole-call `ReturnValue` barriers stop both flows; an `Option<PathBuf>`
validator with `ReturnValue` also stops flow. Loading an extension without
schema errors does not establish that its access path blocks taint.

The bundled Rust barrier implementation (`DataFlowImpl.qll`, `barrierNode`)
selects a reporting expression using `stack.headOfSingleton()`. A field-bearing
summary stack does not identify the whole-call barrier that body flow needs.
This is a barrier limitation: do not mechanically change `Result::Ok` access
paths in source, sink or summary models, which have different interpretations.
Rerun the fixture when upgrading the bundle; this result is version-specific.

A whole-return barrier also covers the error variant. Use it only for a narrow
validator whose success payload is checked, whose other success variants carry
no unsafe value, and whose error payload is used only for diagnostics or
propagation. Error text may contain rejected input; it must never be reused as
a filesystem path (or, for other barrier kinds, argv or a request URL). If that
contract does not hold, isolate validation into a helper returning only the
admitted value, such as `Option<PathBuf>`, instead of marking the aggregate
result safe. Models must name the function's current defining module, including
private submodules; a stale path silently matches no calls.

Run the opt-in behavior fixture with the same prerequisites as the local runner:

```bash
python3 scripts/test-codeql-path-barriers.py
```

It generates a separate Cargo workspace under `.orbit/tmp/`, copies the local
runner, and runs `rust-security-extended` with complete Linux extraction. The
validators intentionally perform no validation, so only the model can stop
taint. An explicit `remote` source model keeps the fixture independent of the
suite's disabled-by-default command-argument threat model. Its unmodelled
control must alert, as must the Result-field cases using `?`, explicit `Ok`
matching, and nested Result/Option extraction. The Result whole-return, nested
whole-return and Option whole-return cases must have no alert. The fixture
checks SARIF locations and retains its generated sources, models, runner log,
`report.json` and the runner's extraction/SARIF evidence in that scratch tree.
It never executes the deliberately unsafe Rust program or adds its models to
production scanning.

The extension schema gate refuses the known ineffective Result-field barrier
form in repository path models. This guard supplements the opt-in flow test;
schema validation alone cannot prove barrier behavior.

## Reusing a verified implementer database

A reviewer working in the implementer's checkout and scratch, with nothing
changed, gets the implementer's run back from the script (see
[Reusing a passing run](#reusing-a-passing-run)). Otherwise, before a
reviewer starts a fresh extraction, inspect the implementer's local
CodeQL evidence. The reviewer may reuse the retained evidence and its results
when the extraction checks passed and the source verification matches the
candidate being reviewed. A passing run removes the database dataset, so
running a different query needs a fresh run. Verify the query selector and CodeQL version, review
the extraction logs for skipped analysis or warnings, and compare the
`codeql resolve files` listing and the paths and contents in `database/src.zip`
with the candidate's production Rust sources and effective configuration.
Inspect the retained `results.sarif` and record the evidence used.

If the evidence is missing, extraction is incomplete, the verified source
tree or configuration differs from the candidate, or the reviewer changes
Rust sources, CodeQL configuration, or model extensions, the database does not
cover the candidate. Run the procedure again against the final candidate tree;
on a non-Linux host, follow [Non-Linux hosts](#non-linux-hosts) for Linux
external evidence.

## Non-Linux hosts

Production modules gated on `#[cfg(target_os = "linux")]`, such as the Linux
sandbox, Landlock and runtime modules, are outside the active cfg on any other
host. A macOS extractor therefore always skips their semantic analysis, and a
run there could never pass the extraction checks above. On a host whose
`uname -s` is not `Linux`, the script exits 3 before preparing anything: no
toolchain, database or query. On Linux every production module is active, so
a skipped one is always the refusal above (exit 1), never a platform exclusion.

Exit 3 is neither a failed check nor confirmation; the check is owed by a Linux
run of the same command. Record the exact command as a `required` validation
with outcome `not_run`, noting the exit-3 platform refusal. An implementer
reports it as not run for that reason. A reviewer whose remaining work is only
this check returns `incomplete` with an `external_evidence` entry of kind
`codeql`, name `Linux CodeQL (rust)`, that exact command, and the result
artifact `evidence/codeql-rust-linux.json`. Settlement then holds the review
for evidence instead of ending it incomplete ([review gate design §4](../design/review-gate/2_design.md)). The hosted `CodeQL / Analyze (rust)`
job runs only for pull requests and pushes to `main` and `agent-main`, so it
cannot supply evidence for a candidate before its PR opens.

A claimed leaf does not depend on its reviewer to name the check. The owner
declares a workspace rule that a Rust change owes it:

```toml
[[review.host_evidence]]
kind = "codeql"
name = "Linux CodeQL (rust)"
paths = ["**/*.rs"]
os = "linux"
command = "scripts/codeql-rust-local.sh --ram 16384 codeql/rust-queries:codeql-suites/rust-security-extended.qls"
artifact = "evidence/codeql-rust-linux.json"
```

On a non-Linux follower, a candidate that changes a matching path owes this
requirement. The reviewer's manifest names it in `owed_external_evidence`. It records the
command `not_run` and never attempts it. Settlement adds the requirement
whatever the report says, so the review holds for the owner's run. It neither
blocks on an `incomplete` verdict whose only gap this is, nor ships a pass the
host could not have run. A Linux follower, or a candidate that changes no
matching path, owes nothing
([design](../design/review-gate/2_design.md#4-what-the-validation-records-establish-orb-11528-orb-11545)).
A before-landing review is the exception only when the rule names the pull
request's hosted check that runs its exact command, with
`hosted_check = "CodeQL / Analyze (rust)"`. The requirement is then owed to the
owner and that check (`owner_owed_evidence`) and does not hold the review; the
owner's landing merges only after the check passed
([runbook](review-gate.md#sandboxed-reviewers-on-a-claimed-leaf)). The hosted
`Analyze (rust)` job in `.github/workflows/codeql.yml` runs the default query
suite, not `rust-security-extended.qls`, so declaring it for the rule above is
the operator's judgement that the hosted run is equivalent enough. Leave
`hosted_check` unset for a custom query (`queries/company-policy.ql`) or a
workspace without hosted CodeQL: the requirement then holds the before-landing
review for the owner's Linux run, as it holds a before-PR review.

### Owner fulfilment

A Linux owner fulfils such a hold without an operator. Its clock sweep
dispatches `review_evidence_fulfilment_pipeline` for each in-progress task
whose latest decision is a hold with only `codeql` (and Linux
`host_sandbox_test`) requirements and no result yet, one run at a time. A
held sandbox test is admitted, run and judged as the
[review gate runbook](review-gate.md#host-sandbox-tests) describes. A claimed leaf's hold reaches the owner too: the leaf
pushes the held candidate to `orbit-evidence/<branch>` on `origin`, and its
settlement keeps the owner's task in progress under the hold.

The owner run requires working Bubblewrap namespaces. Its sweep defers when
Bubblewrap is unavailable, so the hold does not spend an attempt. Within the
Bubblewrap namespace, the host filesystem is read-only, `/tmp` is private,
and the evidence checkout is the only writable task-controlled path. The run takes
these steps:

1. It re-checks that the hold is current.
2. It admits only this script, with nothing but `--ram`, `--toolchain` and one
   query selector, made of characters with no shell meaning. A hold can never
   make the owner run another command.
3. It defers while the state directory's filesystem has less than the job's
   `min_free_mib` free (30 GiB by default).
4. It fetches the held commit from `origin` when needed and checks its tree.
5. It runs the script without a shell, through Bubblewrap, in a standalone
   shallow checkout of the held commit fetched from the owner's repository,
   with `ORBIT_SCRATCH_DIR` inside that checkout. The checkout keeps its own
   Git metadata, so the script's `git` calls resolve without the owner's
   `.git`. The sandbox never writes that `.git`, and does not show it at all
   when the repository is under `/tmp`. Host Cargo download caches are
   read-only because the script uses a run-local `CARGO_HOME`, and `codeql`
   and `rustup` come from the same login-shell `PATH` as required validation.
   The [host toolchain cache](#host-toolchain-cache) is read-only too: a
   valid cached toolchain and existing slot locks are used as is, and a cold
   cache falls back to a per-run install inside the checkout.
   A fresh checkout holds no earlier run, so nothing is reused. The
   checkout is removed afterwards, run directory included.

The result is `passed` only when the script exits zero, reports completed
analysis, and its `results.sarif` has runs and no results. The run then
attaches `evidence/<name>.json` and its log `evidence/<name>.log.json`, which
records the exit, SARIF summary and output tails. Receipt of every result
moves the task to the backlog for a fresh review, which verifies them. When a
host-evidence rule owed every held check, the owner's next run resumes the
held candidate (`resumed_held`). It settles the held review without a
reviewer, as long as the candidate rebuilds to the same tree on the same base.
Delivery then continues to the pull request.

Any other run attaches only the log and leaves the hold in place with a typed
reason:

| Reason | Cause |
| --- | --- |
| `analysis_incomplete` | Incomplete extraction, or no completed analysis or SARIF. |
| `findings_reported` | The analysis reported results; a review must judge them. |
| `tool_missing` | `codeql` or `rustup` is missing. |
| `platform_refused` | The script exited 3. |
| `command_failed` | The script failed for another reason. |
| `timed_out` | The run exceeded three hours. |
| `results_unreadable` | The SARIF could not be read. |
| `command_not_allowed` | The command is not this script with admitted options. |
| `artifact_not_allowed` | An evidence or log path is invalid or canonicalizes to a reserved review artifact. |
| `candidate_unreachable` | The held commit could not be fetched, or its tree differs. |
| `disk_insufficient` | The free space is below `min_free_mib`. |
| `sandbox_unavailable` | Bubblewrap could not provide the required Linux namespace. |
| `not_owner` | This process is a claimed worker or replica, not the task owner. |
| `hold_not_current` | The hold was superseded before the run. |

`disk_insufficient` and `candidate_unreachable` are retried on later ticks, as
is a run that ended without an outcome (an interrupted worker). Each hold gets
at most three runs. Every other refusal needs a new decision. The owner's
attempt decision reads the task's full fulfilment history for that hold;
newer runs for other holds cannot reset its budget or reopen a final refusal.
Every attempt is audited as `review.evidence_fulfilment` and commented on the task.
To lower or raise the disk gate, override `min_free_mib` in a workspace copy
of the job.

## Behavior tests

The behavior tests use stubbed CodeQL, rustup and host platform without downloads. The
stubbed CodeQL enumerates a fixture checkout's sources with the effective
configuration, including build, scratch, and toolchain residue, and an earlier
run under another scratch. They also cover toolchain cache reuse, reinstall
and fallback, a third run queueing for two slots, the thread count, pack
selector reuse, local suite/query dependency edits that force analysis, and
what passing and failing runs keep. Off Linux, the cases supply each missing
Linux command (`flock`, GNU `mv -T`, GNU `nproc`) as a shim and say so on
stderr; a Linux host always runs its own:

```bash
python3 scripts/test-codeql-rust-local.py
```

See GitHub's [directories to scan](https://docs.github.com/en/code-security/reference/code-scanning/workflow-configuration-options#specifying-directories-to-scan)
for `paths-ignore`, its [database analyze reference](https://docs.github.com/en/code-security/reference/code-scanning/codeql/codeql-cli-manual/database-analyze)
for query selectors, SARIF, RAM, and cache options, and the
[CodeQL system requirements](https://codeql.github.com/docs/codeql-overview/system-requirements/)
for Rust prerequisites.

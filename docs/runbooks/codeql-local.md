---
type: runbook
summary: Run local Rust CodeQL for every change touching Rust sources with isolated toolchain preparation, and reject incomplete semantic extraction.
tags: [rust, codeql, security, validation]
paths: ["scripts/codeql-rust-local.sh", ".github/codeql/**"]
last_validated: 2026-10-06
---

# Run local Rust CodeQL for every change touching Rust sources

Use this procedure to validate every change that touches Rust sources before
handoff. For a code-scanning repair, check the identified rule at every affected
location; for other Rust changes, run a relevant Rust query or suite. An
extractor can exit zero after losing semantic analysis; a clean query against
that database is invalid evidence.

## Prerequisites

Use an existing CodeQL bundle (CLI plus Rust query packs), `rustup`, Cargo
proxies on `PATH`, and a working Bubblewrap installation that can create its
Linux namespaces. The owner-side fulfilment runs candidate-controlled scripts
inside Bubblewrap, with the detached evidence checkout as the only writable
task-controlled path. The script does not install CodeQL or global toolchains.
Rust downloads and any Cargo dependency fetches use only the executor's granted
network access. Start in the checkout being validated.

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

`--ram` is in MiB and applies to extraction and analysis. The default is 16384;
a previous targeted query exhausted the heap at 4096. Choose a value appropriate
to the executor's available memory.

Each invocation creates a fresh `codeql-rust-local.*` directory under
`$ORBIT_SCRATCH_DIR` (or `<checkout>/.orbit/tmp` when unset). It isolates
`RUSTUP_HOME`, `CARGO_HOME`, Cargo build output, temporary files, and CodeQL
caches there. Toolchain installation uses
`--profile minimal --component rust-src --no-self-update` and never writes
to the user's `~/.rustup`. The minimal profile is required: a default-profile
install writes rust-docs, and `macro.env.html` matches denyModify `**/*.env.*`
even under the scratch directory. The run directory is printed before preparation,
and retained on success or failure. It contains:

- `toolchain.log`: preparation of the pinned toolchain and rust-src.
- `codeql-config.yml`: the effective configuration used for extraction.
- `extraction.log`, `extraction-problems.log`, `codeql-logs/`, and
  `database/log/`: extraction evidence, including extractor-only warnings.
  `database/src.zip` holds every extracted source file.
- `analysis.log` and `results.sarif`: query execution and results.

## Source selection

CodeQL extracts every `.rs` file under the checkout that the configuration does
not exclude. The shared `.github/codeql/codeql-config.yml` excludes tests,
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
completed. **It does not mean the rule is clean.** Inspect `results.sarif` for
the findings relevant to the selected query and change; for an identified
repair, inspect that rule and each affected location. Retain the full logs,
database and `src.zip` locally under `.orbit/tmp/`. Attach a compact JSON record
with the query selector, CodeQL version, validated source tree, extraction
checks, SARIF findings, and SHA-256 digests of the retained logs and source
archive. Attach only bounded log/listing excerpts (gzip when useful), each
within the artifact tool's 1 MiB limit; never attach `src.zip`, another archive,
or a full source listing. When practical, run the same query on the baseline to confirm it
detects the finding.
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

## Reusing a verified implementer database

Before a reviewer starts a fresh extraction, inspect the implementer's local
CodeQL evidence. The reviewer may reuse the retained database and its results
when the extraction checks passed and the source verification matches the
candidate being reviewed. Verify the query selector and CodeQL version, review
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

### Owner fulfilment

A Linux owner fulfils such a hold without an operator. Its clock sweep
dispatches `review_evidence_fulfilment_pipeline` for each in-progress task
whose latest decision is a hold with only `codeql` requirements and no result
yet, one run at a time. A claimed leaf's hold reaches the owner too: the leaf
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
   The checkout is removed afterwards, run directory included.

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
at most three runs. Every other refusal needs a new decision. Every
attempt is audited as `review.evidence_fulfilment` and commented on the task.
To lower or raise the disk gate, override `min_free_mib` in a workspace copy
of the job.

## Behavior tests

The behavior tests use stubbed CodeQL, rustup and host platform without downloads. The
stubbed CodeQL enumerates a fixture checkout's sources with the effective
configuration, including build, scratch, and toolchain residue, and an earlier
run under another scratch:

```bash
python3 scripts/test-codeql-rust-local.py
```

See GitHub's [directories to scan](https://docs.github.com/en/code-security/reference/code-scanning/workflow-configuration-options#specifying-directories-to-scan)
for `paths-ignore`, its [database analyze reference](https://docs.github.com/en/code-security/reference/code-scanning/codeql/codeql-cli-manual/database-analyze)
for query selectors, SARIF, RAM, and cache options, and the
[CodeQL system requirements](https://codeql.github.com/docs/codeql-overview/system-requirements/)
for Rust prerequisites.

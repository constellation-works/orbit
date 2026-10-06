---
type: runbook
summary: Confirm Rust Code scanning repairs locally with isolated toolchain preparation and reject incomplete semantic extraction.
tags: [rust, codeql, security, validation]
paths: ["scripts/codeql-rust-local.sh", ".github/codeql/**"]
last_validated: 2026-10-06
---

# Confirm a Rust Code scanning repair locally

Use this procedure to check the identified Rust rule at every affected location
before handing off a remediation. An extractor can exit zero after losing
semantic analysis; a clean query against that database is invalid evidence.

## Prerequisites

Use an existing CodeQL bundle (CLI plus Rust query packs), `rustup`, and Cargo
proxies on `PATH`. The script does not install CodeQL or global toolchains.
Rust downloads and any Cargo dependency fetches use only the executor's granted
network access. Start in the checkout being validated.

The default extractor toolchain is **Rust 1.97.0 plus rust-src**, confirmed for
CodeQL **2.27.1**. This is separate from the repository's build toolchain. For
another bundle, inspect its extractor's requested version and pass
`--toolchain <version>`; do not substitute the host's installed stable version.

Run it on Linux, the platform of the hosted CodeQL job. Other hosts cannot
produce a complete run; see [Non-Linux hosts](#non-linux-hosts).

## Procedure

Run a named query or suite. This example runs the bundled Rust security suite;
a targeted query may replace the selector with a `.ql` path or pack selector.

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
caches there. Toolchain installation uses `--no-self-update` and never writes
to the user's `~/.rustup`. The run directory is printed before preparation,
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
run directory must be one of those inputs. Retain both listings with the run
evidence.

## Verification and failures

Exit zero means preparation, extraction-log checks, and query execution
completed. **It does not mean the rule is clean.** Inspect `results.sarif` for
the identified rule and each affected location, and retain the logs, query
selector, CodeQL version, validated HEAD, and `src.zip` listing with the task
evidence. When practical, run the same query on the baseline to confirm it
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
for evidence instead of ending it incomplete ([review gate design §4](../design/review-gate/2_design.md)). A Linux
host fulfils the hold by running that command at the held candidate commit,
inspecting `results.sarif` as above, and attaching the result and its log
artifact; fresh review then verifies them. The hosted `CodeQL / Analyze (rust)`
job runs only for pull requests and pushes to `main` and `agent-main`, so it
cannot supply evidence for a candidate before its PR opens.

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

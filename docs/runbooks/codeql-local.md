---
type: runbook
summary: Confirm Rust Code scanning repairs locally with isolated toolchain preparation and reject incomplete semantic extraction.
tags: [rust, codeql, security, validation]
paths: ["scripts/codeql-rust-local.sh", ".github/codeql/**"]
last_validated: 2026-10-05
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
- `extraction.log`, `extraction-problems.log`, `codeql-logs/`, and
  `database/log/`: extraction evidence, including extractor-only warnings.
- `analysis.log` and `results.sarif`: query execution and results.

The repository CodeQL configuration controls extraction exclusions, and CodeQL
retains repository extension packs in the database for analysis. Hosted
`.github/workflows/codeql.yml` continues to operate independently.

## Verification and failures

Exit zero means preparation, extraction-log checks, and query execution
completed. **It does not mean the rule is clean.** Inspect `results.sarif` for
the identified rule and each affected location, and retain the logs, query
selector, CodeQL version, and validated HEAD with the task evidence. When
practical, run the same query on the baseline to confirm it detects the finding.
Source-side confirmation does not close hosted alerts; closure requires a
hosted rescan.

The script exits nonzero before querying if installation fails, extraction
fails, or console/detailed logs report unavailable semantic analysis, skipped
macro expansion, or any extraction warning/error. Diagnostics include the
prepared version and matching log lines, preserving the requested toolchain and
underlying rustup error when the extractor reports them. If a bundle needs a
different version, rerun with that pinned `--toolchain` value. If scratch is
unwritable or downloads are refused, record the denial as a validation blocker.
Do not treat the incomplete database as confirmation or install into global
rustup storage. A failed query's partial SARIF is also unusable.

The behavior tests use stubbed CodeQL and rustup without downloads:

```bash
python3 scripts/test-codeql-rust-local.py
```

See GitHub's [database analyze reference](https://docs.github.com/en/code-security/reference/code-scanning/codeql/codeql-cli-manual/database-analyze)
for query selectors, SARIF, RAM, and cache options, and the
[CodeQL system requirements](https://codeql.github.com/docs/codeql-overview/system-requirements/)
for Rust prerequisites.

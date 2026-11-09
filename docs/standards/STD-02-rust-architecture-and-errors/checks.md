# Machine checks — STD-02 Rust architecture and errors

The gate that enforces each rule in [STD-02](STD-02.md), or `review-only`. Ship a change
with the gate its rule names, or say it is review-only.

| Rule | Gate |
|---|---|
| R1 | Dependency-direction check (below) |
| R2 | Dependency-direction check (surface and transport crates/modules forbidden in domain); otherwise review-only |
| R3 | Dependency-direction check: grep ban on config-loading and root-discovery calls outside composition |
| R4 | Dependency-direction check |
| R5 | Dependency-direction check (no internal edges) plus a grep ban on `std::fs`/`std::net`/`std::process` in the contract unit |
| R6 | Dependency-direction check in CI (fails on an unlisted edge or an unlisted crate) |
| R7 | Dependency-direction check bans surface crates (`clap`, terminal and log-subscriber crates) in the domain crate; the split decision itself is review-only |
| R24 | A regression test that drives the rule through the shared function rather than through one surface (Orbit: the ship guard's test calls `submit_ship_run` directly); otherwise review-only |
| R25 | review-only |
| R8 | review-only (optionally `unreachable_pub = "warn"`, not in Orbit's baseline) |
| R9 | review-only (Orbit does not gate it; see *Deviations* in [`STD-02.md`](STD-02.md#deviations)) |
| R10 | review-only; an optional grep for `.contains(` on `stderr`, error `to_string()` or message values in classification code is a useful review aid |
| R11 | review-only |
| R12 | Error-translation check (registry of boundary errors → translator in owning unit; no `FooError::X => SurfaceError::Y` at call sites) — adopt once there are two or more translators |
| R13 | `clippy::unwrap_used`, `clippy::expect_used` (+ `-D warnings`) |
| R14 | review-only |
| R36 | Adoptable lint `clippy::fn_params_excessive_bools` with `max-fn-params-bools = 1` catches two or more `bool`s on one function; a single mode `bool` is review-only (at `0` the lint also fires on setters and other data `bool`s) |
| R37 | Default-on `clippy::wrong_self_convention`, `clippy::len_without_is_empty` and `clippy::should_implement_trait` (+ `-D warnings`) catch part of it; the rest is review-only |
| R38 | Default-on `clippy::ptr_arg` and `clippy::from_over_into`; adoptable `clippy::clone_on_ref_ptr`, `clippy::rc_buffer`, `clippy::needless_pass_by_value`; clones that only end a borrow are review-only |
| R26 | review-only; a unit test per error constructor can assert that the resolved inputs appear in the message |
| R27 | Adoptable lints `clippy::wildcard_enum_match_arm` and `clippy::match_wildcard_for_single_variants` (see [Adoptable additions](#adoptable-additions-beyond-orbits-baseline)); not in Orbit's baseline |
| R28 | A load-time test per bounded key (an out-of-range value is refused at load, naming the key); adoptable lints `clippy::cast_possible_truncation`, `clippy::cast_sign_loss`, `clippy::cast_possible_wrap` for the `as` ban; not in Orbit's baseline |
| R29 | Tests that make the source unavailable or failing and assert the explicit unknown state (not 0, empty, healthy or exit 0); a health check's test runs against a stopped or broken target; otherwise review-only |
| R30 | review-only (R27's exhaustive matches keep the outcome enum honest) |
| R31 | A negative test per fail-closed check (unverifiable input → refusal) and per fail-open channel (side-channel failure → primary operation succeeds and the failure is recorded); the class declaration is review-only |
| R32 | A test with one poisoned item among good ones: the good items complete, the bad one is reported by identity, and the summary counts it as a failure |
| R33 | A test per documented partial state that seeds it and asserts the recovery path completes or undoes it |
| R34 | A test per rejected input asserting the fixture root is unchanged afterwards; dry-run/real-run code sharing is review-only |
| R15 | `clippy::print_stdout`, `clippy::print_stderr`, `clippy::dbg_macro`, plus a stream grep guard (below): clippy does not see `writeln!(io::stdout(), …)` |
| R16 | Round-trip tests on persisted fixtures (old document deserializes; removed key warns; write → read → write is byte-stable); grep ban on `serde(default = "…now")` |
| R17 | Retired-path guard in the dependency-direction check; otherwise review-only |
| R18 | review-only |
| R19 | Orphan-module check (every `src/**/tests/*.rs` declared in its `tests/mod.rs`) |
| R20 | review-only |
| R21 | review-only |
| R22 | `cargo clippy --all-targets -- -D warnings` in CI and in the pre-handoff gate |
| R23 | `cargo deny check` in CI |
| R35 | Adoptable lints `clippy::allow_attributes_without_reason` and `clippy::allow_attributes`; a grep ban on `deny(warnings)` in `*.rs` |

## Lint baseline (R13, R15, R22)

This is Orbit's complete `[workspace.lints]` table, copied from `codebases/orbit/Cargo.toml`
(lines 44–55) as of 2026-09-26 — not a subset. In a workspace root `Cargo.toml`:

```toml
# Workspace-wide lint config. Each crate opts in via `[lints] workspace = true`.
# Keep this list short — only rules that are mechanically true everywhere.
[workspace.lints.clippy]
await_holding_lock = "deny"
dbg_macro = "deny"
expect_used = "warn"
print_stderr = "warn"
print_stdout = "warn"
unwrap_used = "warn"

[workspace.lints.rust]
missing_docs = "warn"
```

and in every member's `Cargo.toml`:

```toml
[lints]
workspace = true
```

For a single-crate project, put the same keys in the package `Cargo.toml` as
`[lints.clippy]` and `[lints.rust]`. `await_holding_lock` is STD-03 territory; it is kept
here because it is part of the same table.

Crate-root attributes that complete the baseline (from `crates/orbit-common/src/lib.rs` and
`crates/orbit-cli/src/main.rs`):

```rust
// Library crates / non-output modules: printing is an error, not a warning.
#![deny(clippy::print_stderr, clippy::print_stdout)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
```

In a new binary, deny the print lints crate-wide too and have the one output module write
through locked handles (`writeln!(io::stdout().lock(), …)`) that return `io::Result`. A
closed pipe then arrives as an `io::ErrorKind::BrokenPipe` value the output layer maps to a
silent exit 0 (STD-01 §R13), so the binary needs neither a scoped print allow nor a
broken-pipe panic hook. Because clippy's print lints do not see those `writeln!` calls, pair
them with the stream grep guard below. Orbit instead allows printing crate-wide in
`main.rs` and allows `missing_docs` at 15 of 17 crate roots, so in practice `missing_docs`
is enforced only in `orbit-config` and `orbit-web`. A new repo keeps `missing_docs` on from
the start. Crate-root integration tests (`tests/*.rs`) need their own
`#![allow(clippy::expect_used, clippy::unwrap_used)]`.

These attributes are Orbit's as written. Since v4, R35 wants a reason on each: write
`#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, reason = "…"))]`, or
drop the attributes and set `allow-unwrap-in-tests = true` and `allow-expect-in-tests = true`
in `clippy.toml`.

CI and the pre-handoff gate run:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings   # single crate: drop --workspace
```

## Adoptable additions (beyond Orbit's baseline)

These lints are **not** in Orbit's table and are not part of R22's required baseline. They
mechanize R27 and R28 for a repo that wants the gate; each name was checked with
`cargo clippy --explain <lint>` on clippy 0.1.96. Add them to the same
`[workspace.lints.clippy]` table (single crate: `[lints.clippy]`):

```toml
# STD-02 §R27 — no wildcard arm on an enum match (restriction group; also fires on
# foreign enums, so scope an #[allow] with a reason where a wildcard is required).
wildcard_enum_match_arm = "warn"
match_wildcard_for_single_variants = "warn"
# STD-02 §R28 — narrowing `as` casts must be checked conversions (pedantic group).
cast_possible_truncation = "warn"
cast_sign_loss = "warn"
cast_possible_wrap = "warn"
# STD-02 §R35 — every suppression is #[expect(..., reason = "...")]; #[allow] only with a
# reason (restriction group; needs rust-version 1.81+). A repo with many reasonless
# #[allow]s can start with allow_attributes_without_reason alone.
allow_attributes_without_reason = "warn"
allow_attributes = "warn"
# STD-02 §R36 — two or more bools on one function (pedantic group); threshold in clippy.toml.
fn_params_excessive_bools = "warn"
# STD-02 §R38 — visible Rc/Arc clones, no Rc<String>, no by-value params that are only read.
clone_on_ref_ptr = "warn"
rc_buffer = "warn"
needless_pass_by_value = "warn"
```

with, in `clippy.toml`:

```toml
max-fn-params-bools = 1
```

And a grep ban for R35's "no `deny(warnings)` in source":

```sh
if grep -rnE --include='*.rs' 'deny\(warnings\)' crates; then  # single crate: src
  echo "#![deny(warnings)] in source (STD-02 §R35)" >&2; exit 1
fi
```

These were checked with `cargo clippy` 0.1.98 on 2026-10-28 (`max-fn-params-bools = 0`
fired on a `set_on(&mut self, on: bool)` setter, hence `1`).

And a grep ban for R16's "no fabricated timestamps", suitable for the same CI job:

```sh
if grep -rnE --include='*.rs' 'serde\(default = "(chrono::)?Utc::now"' crates; then  # single crate: src
  echo "fabricated timestamp default (STD-02 §R16)" >&2; exit 1
fi
```

## Review checklist (rules no lint can check)

Use this when reviewing a Rust diff against STD-02, after the gates above are green. Check
only the changed lines and what they break.

- **Failure paths.** A new error path names verified causes and resolved inputs (R26); a
  `map_err(|_| …)` that drops the source, or a `String` error in library code (R10); a
  panic reachable from input (R13); unknown read as 0, empty or success (R29); one bad item
  aborting a batch (R32); a side effect before validation (R34).
- **Types and shapes.** A wildcard arm on a domain enum (R27); a bare `String` id that
  crosses a function boundary (R14); a persisted shape changed incompatibly or a fabricated
  default (R16); a `pub` that only a test needed (R8, R19).
- **Signatures and names.** A mode `bool` on a shared function (R36); `get_`, a mismatched
  `as_`/`to_`/`into_`, or a `Manager`/`Util` name (R37); `&String`/`&Vec<T>`, a clone that
  only ends a borrow, `Rc`/`RefCell` in a shared signature, or a sentinel for absence
  (R38).
- **Suppressions.** A new `#[allow]`, or an `#[expect]` without a reason (R35).
- **Structure.** A new edge against the layer order (R1–R6); logic in a surface (R2, R24).

Report each finding as `STD-02 §Rn — <what is wrong> — <path>:<line>` with the smallest
code change that fixes it, grouped by severity:

- **Must fix:** a bug, a reachable panic, a broken persisted or public contract, or a MUST
  rule broken by new code.
- **Should fix:** a SHOULD rule or a MUST rule's cost that is real but not yet a bug (a
  worse error message, a harder test).
- **Consider:** naming and polish, and anything that looks deliberate; say the trade-off.

Group repeats into one finding with every location. Keep the list proportionate, and name
what the change already does well so the author knows what to keep.

## Stream grep guard (R15)

Only the output module may name the standard streams. A minimal guard, assuming that module
is `crates/<tool>/src/output/` (single crate: `src/output/`):

```sh
#!/usr/bin/env bash
# scripts/check-terminal-guard.sh — STD-02 §R15 (and STD-01's stream rules).
set -euo pipefail
cd "$(dirname "$0")/.."
OUTPUT_DIR=crates/mytool/src/output
PATTERN='io::stdout|io::stderr|println!|eprintln!|print!|eprint!|is_terminal|IsTerminal'
hits=$(grep -rnE --include='*.rs' --exclude-dir=tests "$PATTERN" crates \
  | grep -v "^$OUTPUT_DIR/" | grep -vE '^[^:]+:[0-9]+:[[:space:]]*//' || true)
if [[ -n "$hits" ]]; then echo "$hits" >&2; echo "std streams belong to $OUTPUT_DIR only" >&2; exit 1; fi
```

The constellation Rust CLI template (`operations/templates/rust-cli/` in the constellation
root) ships the full version as `scripts/check-terminal-guard.sh`. List any justified
exception (R15) as an extra `grep -v` with a comment giving its reason.

## Dependency-direction check (R1–R7, R17)

**Workspace, from `cargo metadata`.** Port the shape of
`codebases/orbit/scripts/check-dependency-direction.sh`: an explicit allowlist of internal
dependencies per crate, read from `cargo metadata --no-deps`, that fails on (a) a workspace
crate with no policy entry, (b) any internal edge not on that crate's list, and (c) a
dev-only edge used as a normal dependency. Keep the layer table in the architecture doc and
the allowlist in the same change (R6). Orbit additionally pins a few crates with a
manifest-parsing test (`crates/orbit-mcp/tests/dep_boundary.rs`); the script alone is
sufficient. This variant needs cargo and a JSON parser.

**Workspace, from the manifests alone.** For a small workspace, or a fast CI lane that runs
before anything compiles, read the non-dev dependency tables straight out of each
`crates/*/Cargo.toml` with `awk`. It needs no build, no network and no `jq`:

```sh
#!/usr/bin/env bash
# scripts/check-dependency-direction.sh — STD-02 §R1–R7 from Cargo.toml files only.
set -euo pipefail
cd "$(dirname "$0")/.."
PREFIX=mytool; fail=0
err() { echo "dependency-direction: $*" >&2; fail=1; }
policy() { # sets ALLOWED (internal deps) and BANNED (external deps); unknown crate = no policy
  case "$1" in
    mytool-core) ALLOWED="";            BANNED="clap tracing-subscriber anstream crossterm" ;;
    mytool)      ALLOWED="mytool-core"; BANNED="" ;;
    *) return 1 ;;
  esac
}
declared_deps() { # names in [dependencies], [build-dependencies] and target-specific tables
  awk '/^\[/ { in_deps = ($0 ~ /^\[(target\..*\.)?(build-)?dependencies\]$/); next }
       in_deps && /^[A-Za-z0-9_-]+[[:space:]]*(\.workspace)?[[:space:]]*=/ {
         n = $1; sub(/\.workspace$/, "", n); sub(/=.*/, "", n); gsub(/[[:space:]]/, "", n); print n }' "$1"
}
has() { local w; for w in $2; do [[ $w == "$1" ]] && return 0; done; return 1; }
for manifest in crates/*/Cargo.toml; do
  crate=$(awk -F'"' '/^name[[:space:]]*=/ { print $2; exit }' "$manifest")
  policy "$crate" || { err "crate '$crate' has no policy"; continue; }
  while IFS= read -r dep; do
    if [[ $dep == "$PREFIX" || $dep == "$PREFIX"-* ]]; then
      has "$dep" "$ALLOWED" || err "$crate must not depend on internal crate $dep"
    elif has "$dep" "$BANNED"; then err "$crate must not depend on $dep"; fi
  done < <(declared_deps "$manifest")
done
exit "$fail"
```

It does not see dependencies declared in dotted-key form (`dependencies.foo = …`) or
inside an inline table on one line; keep manifests in the conventional one-table-per-section
shape, or use the `cargo metadata` variant. Module bans (the single-crate block below) can
follow in the same script, as the Rust CLI template's
`scripts/check-dependency-direction.sh` does.

**Single crate.** Layers are modules, so the check is a set of `rg` bans over
`src/`, excluding test code. A minimal version for `domain` / `app` / `cli` layers:

```sh
#!/usr/bin/env bash
# scripts/check-dependency-direction.sh — STD-02 §R1–R6 for a single-crate CLI.
set -euo pipefail
cd "$(dirname "$0")/.."
fail=0
ban() { # ban <dir> <pattern> <message>
  if rg -n "$2" "$1" -g '*.rs' -g '!**/tests/**'; then echo "$3"; fail=1; fi
}
ban src/types  'crate::(domain|app|cli)|std::(fs|net|process)' "types must be I/O-free leaves"
ban src/domain 'crate::(app|cli)|\bclap\b'                     "domain must not import app or surface code"
ban src/app    'crate::cli'                                     "composition must not import surfaces"
ban src/domain 'std::env::(current_dir|home_dir|var)'           "domain receives resolved roots and config"
for retired in src/legacy; do [[ -e "$retired" ]] && { echo "retired path exists: $retired"; fail=1; }; done
exit "$fail"
```

Wire it into the same CI job as clippy. Orbit's `orbit-core` and `orbit-store` blocks in its
script (lines 183–234) are the multi-module version of exactly this.

## cargo-deny (R23)

Install a pinned `cargo-deny`, check in `deny.toml`, and run `cargo deny check` in CI. The
policy sections below are copied from `codebases/orbit/deny.toml` (subset: Orbit's
`ignore` entry and license comments are omitted — start with an empty `ignore` and add the
licenses your tree actually needs):

```toml
[advisories]
yanked = "deny"
ignore = [
    # { id = "RUSTSEC-YYYY-NNNN", reason = "<why it does not apply>. Re-review YYYY-MM-DD." },
]

[licenses]
allow = ["MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Unicode-3.0"]
confidence-threshold = 0.9

[bans]
multiple-versions = "allow"
wildcards = "allow"

[sources]
unknown-registry = "deny"
unknown-git = "deny"
```

A sandboxed or offline runner needs a writable advisory DB; Orbit's
`scripts/cargo-deny.sh` shows the wrapper (`--disable-fetch`, custom `db-path`).

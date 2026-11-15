# Why — STD-02 Rust architecture and errors

Why each rule in [STD-02](STD-02.md) exists, tied to the incident behind it where there
was one. Read the entry for a rule when its intent is unclear or before deviating. The
rules themselves are in `STD-02.md`; this file explains them and adds none.

Decision citations of the form `area:n` point at
`codebases/orbit/docs/design/<area>/4_decisions.md` line *n*. Frictions are Orbit's unless
marked *(polaris)*.

R36–R38 and R35 were adopted in v4 from the BeautifulRust agent skills
(github.com/morganlinton/BeautifulRust, MIT, reviewed 2026-10-28), which distil the Rust API
Guidelines, Effective Rust, the Google Rust Style Guide, Rust Design Patterns and
Microsoft's Pragmatic Rust Guidelines. Only rules that STD-02 lacked and that do not
contradict it were taken; its advice to use `anyhow` in application code was not (R10).

- **R1.** A declared order turns "where does this go?" into a lookup. Orbit's 17 crates stay
  navigable because every crate sits on one named tier and "lower layers never depend on
  higher ones" is the whole rule; agents new to a repo follow a written order and violate a
  tacit one.
- **R2.** Orbit's CLI, MCP server, dashboard and agent tool hosts are four surfaces over the
  same operations. Where logic leaked into a surface it had to be re-plumbed per surface;
  ORB-10016 split the command layer out of the `orbit-core` god-crate into `orbit-cmd`
  (depending on Core, never the reverse) to get composition out of the kernel.
- **R3.** A unit that reads config or `$HOME` itself cannot be tested against a fixture root
  and silently diverges from what composition resolved. Orbit's config crate takes explicit
  roots, and its dependency check fails if Core's runtime calls `ResolvedConfig::load`.
- **R4.** A mechanism that imports a feature drags that feature into every consumer and
  creates cycles the moment the feature needs the mechanism. Orbit's `orbit-exec` and
  `orbit-policy` depend only on the two foundation crates.
- **R5.** Contract types are depended on by everything. If they do I/O, every consumer
  inherits side effects and heavy dependencies, and the types stop being safe to construct
  in tests. `orbit-types` does no I/O and its serde shapes double as persisted contracts.
- **R6.** An undocumented edge is how layering erodes: one convenient import at a time.
  Orbit's `check-dependency-direction.sh` rejects any edge not in its allowlist and any new
  crate without a policy; `AGENTS.md` makes a new edge an `ARCHITECTURE.md` change too.
- **R7.** Splitting by category multiplies manifests, re-exports and orphan-rule walls
  without buying isolation. Orbit's north-star bearing 5 records that crate splits are
  justified by compile-graph and dependency-direction needs; `orbit-store` stays one crate
  with a directional internal module graph checked by the same script. The two-crate
  default for a new CLI came from building the constellation Rust CLI template (ORB-13117):
  "the domain never imports clap or the terminal" is only machine-checkable as a manifest
  edge, and a single crate can only approximate it with grep bans.
- **R24.** A rule spelled twice drifts, and a guard placed on one surface is routed around
  by the next. Dashboard writes skipped `validate_for_set` (ORB-12744); a guard skipped the
  dashboard API (ORB-12199); a fix landed on the CLI while MCP still capped silently
  (ORB-12195); a `/healthz` variant skipped the Host gate, so DNS rebinding still worked
  (ORB-12531); one backend method sat outside `in_boundary` (ORB-12545); two selector
  grammars coexisted (ORB-11723); also ORB-12158 and F2026-07-116 ("nothing cross-checked
  the two"). Orbit's decisions say it plainly: "Two independent spellings of one rule is
  the defect" (activity-job:1062); a guard duplicated per adapter means "a third
  submission adapter would silently start without it" (activity-job:1229); any per-command
  guard "could be routed around by the next entry point" (operations-as-data:156); a
  duplicated table "could drift" (policy-sandbox:332). See also mcp-bridge:101 and
  auditability:153.
- **R25.** Prompt text is advice to a component that is free to skip it. F2026-07-102
  concluded "prompt text is the wrong enforcement layer" after three failed runs;
  F2026-07-125 found an agent instructed to produce an artifact it was structurally unable
  to produce; and when the execution summary was a prompt instruction, "agents skip it
  often, and every run that skipped it wedged at commit" (activity-job:1479), so Orbit now
  derives it from the change. SHOULD rather than MUST: many projects have no instruction
  layer, and some obligations cannot be computed.
- **R36.** `report(true)` cannot be read at the call site and cannot grow a third mode
  without breaking every caller; a two-variant enum names the choice and extends cleanly.
  Clippy's `fn_params_excessive_bools` cannot tell a mode from data, so the rule is mostly
  review-only. Data `bool`s are
  exempt because an enum there adds a type without adding meaning.
- **R37.** Shared naming conventions let a reader predict an API without opening it: `as_`,
  `to_` and `into_` tell the caller what a conversion costs and whether it consumes the
  value, and a getter without `get_` matches the standard library (API Guidelines C-CONV,
  C-GETTER, C-CTOR). `Manager`/`Service`/`Util` names say nothing about what a type does and
  attract unrelated code, the type-level form of R7's taxonomy crates. SHOULD, because
  renaming an established name is a churn cost a reviewer may reasonably decline.
- **R38.** `&String` and `&Vec<T>` force callers to own exactly that container; the general
  borrow accepts literals, slices and borrows alike. A clone added to quiet the borrow
  checker silently splits one value into two, so a later write lands on the copy. `Rc` or
  `RefCell` in a shared signature moves borrow checking to run time for every caller. A
  sentinel `""` or `-1` is R29's "unknown reads as a value" in miniature. SHOULD, because
  each item has legitimate exceptions a reviewer can see.
- **R8.** Every `pub` item is surface someone else can couple to. Private-by-default keeps
  refactors local; ORB-10016 trimmed `orbit-core`'s root re-exports to the consumer-justified
  set for this reason.
- **R9.** One declaration per dependency keeps versions and features from drifting between
  members and makes upgrades a one-line change.
- **R10.** A typed enum lets callers branch on the failure (retry, report as input error,
  abort) instead of parsing messages. Stringly errors lose the kind at the first hop, and
  classifying by substring misfires as soon as wording changes or two classes share text:
  Orbit derived agent timeouts from stderr text until ORB-11701 moved them to the
  supervisor's verdict; ORB-12792 was a URL substring match; F2026-09-151 found the same
  errno text for opposite classes; also ORB-11802 and ORB-12335.
- **R11.** Surfaces map errors to exit codes, HTTP statuses and audit outcomes. One
  surface type means one mapping; `OrbitError` is `#[non_exhaustive]` and carries a size
  budget because it rides in every `Result` in the workspace. The two-crate CLI note came
  from the template build (ORB-13117), where the surface type is private and the attribute
  protects nothing.
- **R12.** Centralising the kind→variant mapping in one place keeps the surface error
  coherent and lets internal code keep the rich type. ORB-10013 added
  `check-error-translation.sh` after a caller crate (`orbit-mcp`) grew a private translator
  for another crate's error. The downstream-surface form was added after the template
  build (ORB-13117) found that "next to the owning error" is impossible when the owner
  cannot depend on the surface crate.
- **R13.** A panic at a boundary turns a recoverable condition (bad input, missing file,
  lost race) into a crash with no error path for the caller or the surface. Orbit keeps
  `unwrap_used`/`expect_used` on for production and exempts tests with `cfg_attr(test)`. Since v4
  an invariant `expect` is marked with `#[expect(clippy::expect_used, reason = …)]` rather
  than a scoped `#[allow]` and a comment, so the invariant travels with the suppression
  (R35).
- **R14.** Validate-then-use leaves every callsite to remember the check and admits
  time-of-check/time-of-use bugs; a validated type carries the proof. Orbit's own
  `docs/design-patterns/newtype.md` records that it has no production private-inner
  validated newtype today and still uses `validate_*(&str)` helpers for identities — a gap,
  hence SHOULD rather than MUST.
- **R26.** "An error that names causes it has not checked converts a one-command fix into a
  diagnosis cycle" (F2026-07-112). Orbit's shared commit-step message "cost three diagnosis
  cycles, two of which reached confidently wrong conclusions" (activity-job:1029), and its
  replacement "asserts no cause it did not measure" (activity-job:1022). A lost index was
  reported as `invalid_input` (ORB-12172); recovery advice that was itself destructive
  (ORB-12131) shows why a remedy must be safe; F2026-07-055 asked for the effective `PATH`
  in the message. Also ORB-10927, F2026-08-106 and policy-sandbox:315 (fail "at a layer that
  knows why").
- **R27.** A wildcard arm silently files every future variant under whatever the default
  was. Orbit's dependency semantics partition `TaskStatus` between two predicates, and its
  decision records the cost: a variant added to neither "is silently treated as a
  legitimate wait forever" (activity-job:1270), which an exhaustive match turns into a build
  error. ORB-10958 is the same shape outside an enum: a redaction allowlist missed new
  writers.
- **R28.** A value that is only checked when it fires fails at the worst time and far from
  its source. ORB-11734 rejected an out-of-range `every_minutes` whose slot arithmetic would
  otherwise overflow; ORB-11726 found a large YAML value that could panic the whole sweep;
  "load-time validation makes a broken reference visible on the next sweep instead of at
  fire time" (routines:106).
- **R29.** A missing value rendered as zero or healthy is a false statement that nobody
  investigates. Orbit's dashboard rendered unavailable failure metrics "as a measured zero"
  (ORB-11201, ORB-11207); tasks vanished silently (ORB-12151); a panicked worker was dropped
  (ORB-11605); doctor reported healthy while a default was missing (ORB-11791); the parent
  could not tell a review that never started from a failed one (ORB-10606); a process "still
  exited 0" (F2026-07-002) or reported "`completed: true` while having done nothing"
  (F2026-07-155); a silent null "provided no diagnostic" (F2026-07-008); and in F2026-08-017
  a merge went through unreviewed. Also ORB-12335, F2026-07-167 and F2026-08-047. The
  decisions: "0% can never stand in for no data" (user-interface:205); "a probe that cannot
  answer must never be read as proof of death" (auditability:492); unpriced models "degrade
  to unknown rather than a silently wrong number" (auditability:435). Health checks that
  read flags fail the same way: "enabled state and successful manager command exits are
  insufficient for health" (routines:208); the sandbox preflight "actually executes the
  sandbox helper rather than merely checking that the binary is installed"
  (policy-sandbox:294); a review was never minted across about 14 deliveries while nothing
  alarmed (F2026-09-200); a full disk froze the server (F2026-09-122).
- **R30.** An operator who cannot tell a wait from a dead end polls forever, and one who
  cannot tell an infrastructure failure from a verdict re-litigates the verdict. Orbit's
  admission now refuses dead-end dependencies instead of waiting on them
  (activity-job:1251), and review startup failure is classified separately from reviewer
  rejection so operators can distinguish them "without opening the child run"
  (activity-job:808); ORB-10606 is the incident.
- **R31.** Guessing through unverifiable security state converts an error into a breach;
  failing the main operation on a telemetry write converts a cosmetic gap into lost work.
  Fail closed: a program allowlist failed open when an activity omitted it (ORB-10959); in
  F2026-09-203 a failed `/proc` read resolved to "ordinary local caller with no allowlist"
  for about 50 minutes; also ORB-11514 and ORB-12789. Orbit's decisions: "all ambiguous or
  changed remote state fails closed" (activity-job:974); "Durable registry input fails
  closed" (host-registry:57); "Orbit verifies it and refuses; it does not silently degrade"
  (policy-sandbox:307). Fail open: "never fail a run on a telemetry write" (commit
  f7af3890f, ORB-10367); fail open when managed-asset reconciliation cannot write a
  read-only manifest (ORB-10840).
- **R32.** One malformed entry should cost one entry. The dashboard refused to start over
  one bad workspace (ORB-11386); one `/proc` failure aborted every runtime open (ORB-12607);
  one large YAML value could panic the whole sweep (ORB-11726); an unavailable systemd bus
  produced a 500 (ORB-12517); also F2026-07-098. The opposite failure hides the loss: a
  polaris security audit reported a clean auth section because a date-format defect made a
  sub-check fail silently (*(polaris)* F2026-07-021, F2026-07-024), and ORB-11801 dropped all
  subprocess output.
- **R33.** A recovery path that demands a clean state strands exactly the cases it exists
  for. A rebase-recovery checkpoint "hard-fails every resume instead of redo[ing]" the rebase
  (ORB-12015); ORB-12575 is the same shape; ORB-12128 stranded an already-disabled tool.
- **R34.** Validation after the first write leaves a half-built state for the next run to
  trip over. `orbit init` seeded the global root before validating `--task-prefix`,
  "leaving a partial root on failure" (ORB-12112); an update failed "only after the
  executable is replaced" (ORB-12612); a pipeline merged "to agent-main before it enforces
  the execution_summary precondition" (F2026-07-014); a dry run disagreed with the real tick
  (ORB-12337). Also ORB-10683, F2026-07-055 and F2026-07-116. An incompatible failure hook
  was discovered "*inside* it … the worst possible time" (activity-job:1042); the sandbox
  preflight now runs the real helper before dispatch (policy-sandbox:294).
- **R15.** `print!` bypasses filtering, structure, redaction and rotation, and corrupts
  stdout for callers parsing command output. Orbit denies print lints in library crates and
  allows them only in the CLI binary, with a comment saying why. The template build
  (ORB-13117) found that `clippy::print_stdout` does not see `writeln!(io::stdout(), …)`, so
  the lint alone does not keep stdout in one module; the stream names are banned too.
- **R16.** Persisted state outlives the binary that wrote it: an older or newer binary will
  read it. Orbit keeps the lexical index file named `semantic.db` so persisted paths stay
  valid, and warns about and ignores removed config keys so an existing `config.toml` keeps
  loading. A fabricated value is worse than a missing one because nothing marks it as a
  guess: Orbit stamped `Utc::now()` into `created_at`/`updated_at` on every read of a
  record that lacked them (ORB-11735); `ActorIdentity` did not round-trip losslessly
  (ORB-11731); a `pr_number` flipped between string and number (ORB-12640). Ambiguous
  evidence should "honestly preserve uncertainty instead of fabricating an exit"
  (auditability:513).
- **R17.** Dormant code still has to be understood by every reader of the paths it sits in.
  Orbit removed the unused operation-mode authorization layer end to end (UI, CLI, Core,
  config keys, tables, design folder) in ORB-12769..12772 rather than keep it behind a
  default, keeping only the load-time warning for removed keys; its dependency check fails
  if a retired module path reappears.
- **R18.** Large files hide multiple responsibilities and make review diffs unreadable.
  Orbit treats ~800 lines as a signal to split, not a hard limit (23 non-test files exceed
  it today), so the rule is SHOULD. Length is the prompt, responsibility is the test: a
  long file that does one closely related job stays whole, because splitting it would only
  scatter one idea across several files (v3, Daniel, 2026-09-26).
- **R19.** A sibling test module is not a child of the source module, so it cannot reach
  private items: the file layout itself enforces "test through the exposed seam" and keeps
  `#[cfg(test)]` noise out of production listings. F2026-08-108 found test files that were
  never declared in `tests/mod.rs` and silently compiled to nothing; Orbit now checks module
  reachability.
- **R20.** Cargo compiles each crate-root test as a separate binary against the public
  surface — right for end-to-end tests, wrong (and slow) for unit tests.
- **R21.** A text-matching test breaks on every wording edit and proves nothing runs; a test
  pinning crew, model or schedule turns every ops edit into red CI. Orbit's agent guide bans
  both, with a narrow exception for structural safety guards that name what they protect.
- **R22.** The lint table is the part of these rules a compiler can check. `warn` keeps
  local iteration unblocked; `-D warnings` in CI makes it binding.
- **R23.** Advisory, license and source drift enters through `Cargo.lock` without anyone
  editing code. Orbit gates it in CI (ORB-00416, ORB-11983) with time-boxed, justified
  exceptions.
- **R35.** A reasonless `#[allow]` outlives the code that needed it and hides new
  instances of the same lint in that item. `#[expect]` with a reason states the exception
  where a reviewer reads it and turns into a warning once the lint no longer fires, so
  stale suppressions clean themselves up. `#![deny(warnings)]` in source breaks the build on
  every toolchain that adds a warning; CI's `-D warnings` (R22) gives the same strictness
  without that. Orbit carried 459 reasonless `#[allow]`s at v4, which is why existing ones
  are fixed on touch rather than all at once.

# Changelog — STD-02 Rust architecture and errors

Version history of [STD-02](STD-02.md). Adopting repos pin the `version:` in its
frontmatter as `STD-02@<version>`; a version changes only when a rule is added,
removed or changed in meaning.

- **4** (2026-10-28, Daniel; adopted from the BeautifulRust agent skills, reviewed
  2026-10-28).
  - Normative, new cluster "Idiom and naming", binding new and changed code: R36 no boolean
    mode parameters on functions callable outside their module (MUST; data `bool`s and
    private helpers exempt); R37 Rust API naming (SHOULD); R38 signatures borrow the most
    general type and own only what they keep, no borrow-ending clones, no sentinels
    (SHOULD).
  - Normative, new rule in Gates: R35 lint suppressions are `#[expect(…, reason = …)]` on
    the narrowest item, `#[allow]` only for generated, macro or target-dependent cases and
    still with a reason, and no `#![deny(warnings)]` in source.
  - Normative, changed rule: R13's invariant `expect` uses `#[expect(clippy::expect_used,
    reason = …)]` instead of a scoped `#[allow]` with a comment.
  - Machine checks gained the R35, R36 and R38 adoptable lints, a `deny(warnings)` grep ban
    and a review checklist with severity tiers. The lint baseline block is unchanged.
  - Not adopted from the source: `anyhow` in application code (contradicts R10), clippy's
    `pedantic` group and `missing_docs` everywhere (R22 keeps the baseline short).
- **3** (2026-09-26, Daniel).
  - Normative, changed rule: R18 treats file size as a heuristic. Past about 800 lines a
    file SHOULD be checked for multiple responsibilities and split along them; closely
    related functionality MAY stay in one file when a split would only scatter it. v2 read
    as "split when past ~800 lines" on length alone.
- **Editorial (2026-09-26, version unchanged):** split into a directory. `STD-02.md`
  keeps the rules, *Applies to* and *Deviations*; *Why*, *Machine checks*,
  *Exemplars* and this changelog moved to `why.md`, `checks.md`, `exemplars.md` and
  `CHANGELOG.md`. No rule changed.
- **2** (2026-09-26, ORB-13126).
  - Normative, new rules from `projects/standards/tacit-extraction-2026-09-26.md`
    (disposition on ORB-13088): R24 one definition per rule at the chokepoint (T6); R25
    obligations in deterministic code, SHOULD (A2); R26 errors state verified causes,
    resolved inputs and a runnable remedy (T13); R27 exhaustive matches on domain enums
    (A5); R28 config validated at load with checked conversions (E4); R29 unknown never
    reads as zero, empty, success or healthy, and health checks probe real state (T2 with
    O2); R30 distinct failure classes get distinct terminal states (E3); R31 fail closed on
    integrity, fail open on side channels, declared per check (T5); R32 one bad item is
    isolated, reported and counted (E2 with E6); R33 recovery paths accept the states they
    exist to fix (E5); R34 preconditions before the first side effect, with preflight
    sharing the real run's checks (T15).
  - Normative, extended rules: R10 classifies failures from structured data, never message
    or stderr substrings (E1). R16 requires lossless round trips and forbids fabricating an
    unknown value on write or deserialize (P4).
  - Normative, clarified from frictions found building the constellation Rust CLI template
    on v1 (ORB-13117): R7 says a new CLI SHOULD start as two crates (domain library plus CLI
    surface), while other projects still start as one. R11 requires `#[non_exhaustive]`
    only when the surface type is public. R12 accepts a single `#[from]`/`From` impl on a
    downstream surface type as the one translator. R15 bans naming `io::stdout()` or
    `io::stderr()` outside the output layer, with a stream grep guard.
  - Editorial: plain-line cluster labels became `###` headings; the Errors cluster is now
    "Errors and domain types", and a new "Failure semantics" cluster holds R29–R34.
    "Applies to" is a tag list with rule scopes. The lint guidance recommends locked-handle
    writes over a scoped print allow. Machine checks gained adoptable lints (beyond Orbit's
    baseline), a stream grep guard and a manifest-only dependency-direction variant. The
    lint baseline block is unchanged.
- **1** (2026-09-26). First publication: R1–R23.

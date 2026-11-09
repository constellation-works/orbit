# Changelog — STD-01 CLI surface

Version history of [STD-01](STD-01.md). Adopting repos pin the `version:` in its
frontmatter as `STD-01@<version>`; a version changes only when a rule is added,
removed or changed in meaning.

- **Editorial (2026-09-26, version unchanged):** split into a directory. `STD-01.md`
  keeps the rules, *Applies to* and *Deviations*; *Why*, *Machine checks*,
  *Exemplars* and this changelog moved to `why.md`, `checks.md`, `exemplars.md` and
  `CHANGELOG.md`. No rule changed.
- **v2 (2026-09-26):**
  - Added:
    - R26: argument ids never shadow a global argument, as a clap note (C1).
    - R27: EOF at a prompt is an error, SHOULD (C4).
    - R28: overrides are honored on every path or rejected (T1).
    - R29: caller input is never silently dropped or coerced (T3).
    - R30: success means the effect happened and names its target (C3).
    - R31: read-only commands never write (T16).
    - R32: identifiers round-trip (C2).
    - R33: no hidden filters, and filters apply before the limit (C7).
    - R34: truncation is explicit in the payload (T12).
    - R35: flag and config-key deprecation window (C5).
    - R36: advertise only what a runtime path implements (A3).
    - A new "Inputs and effects" cluster.
  - Clarified:
    - R3 and R35: renaming or removing a flag is explicitly a breaking change.
    - R3 and R7: `--json` is the one sanctioned shorthand for `--format json`, and a
      conflicting pair is a usage error checked after parse.
    - R10: a top-level shape change is breaking.
    - R15 and R17: the width comes from the one resolver, no width means no truncation,
      `COLUMNS`-only is compliant, and clap is built without `color`/`wrap_help`.
    - R16: it governs list-shaped output, a `show` detail view MAY be key-value, and an
      empty list is `[]` under `--json` and nothing under NDJSON.
    - R19: a usage error may be multi-line and plain when the mode cannot yet be
      resolved; pre-scanning the raw arguments is how to get JSON usage errors.
  - Editorial: `###` cluster headings, the shared "Applies to" form, and deviation
    examples using `STD-01@<version>`.
  - Sources: ORB-13088 (candidate disposition), ORB-13117 (template-author feedback),
    ORB-13118 (skill-author feedback).
- **v1 (2026-09-26):** initial publication, R1–R25.

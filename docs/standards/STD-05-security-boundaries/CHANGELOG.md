# Changelog — STD-05 Security boundaries

Version history of [STD-05](STD-05.md). Adopting repos pin the `version:` in its frontmatter as `STD-05@<version>`; a version changes only when a rule is added, removed or changed in meaning.

- **Editorial (2026-11-11, version unchanged):** repointed exemplars that had moved in Orbit (R2, R3, R4, R5, R7, R8, R9, R13, R14, R21, R22): split modules (`authorization/`, `fs/io/`, `storage/sqlite/`, `artifact_redaction/`, `redaction/`), plugin host code now in `orbit-runtime`, the R2 test retired in favour of the `self_reported_actor` type's module doc, and R9's state-directory check now in `orbit-cmd`. No rule changed. ORB-24166.
- **Editorial (2026-09-26, version unchanged):** split into a directory. `STD-05.md` keeps the rules, *Applies to* and *Deviations*; *Why*, *Machine checks*, *Exemplars* and this changelog moved to `why.md`, `checks.md`, `exemplars.md` and `CHANGELOG.md`. No rule changed.
- **v1** (2026-09-26): first publication. R1 to R25 from tacit-extraction candidates T14, S1 to S7 and P5 (ORB-13088 disposition), plus R15 and R20 from Orbit's positioning non-negotiables. ORB-13129.
- **Editorial (2026-11-23, version unchanged):** the frontmatter drops `status`, which nothing read (ORB-27289).

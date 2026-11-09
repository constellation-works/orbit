# Changelog — STD-03 Concurrency and process safety

Version history of [STD-03](STD-03.md). Adopting repos pin the `version:` in its
frontmatter as `STD-03@<version>`; a version changes only when a rule is added,
removed or changed in meaning.

- **Editorial (2026-09-26, version unchanged):** split into a directory. `STD-03.md`
  keeps the rules, *Applies to* and *Deviations*; *Why*, *Machine checks*,
  *Exemplars* and this changelog moved to `why.md`, `checks.md`, `exemplars.md` and
  `CHANGELOG.md`. No rule changed.
- **v2 (2026-09-26, ORB-13127).** Folded in the accepted candidates from the ORB-13088 tacit
  extraction (disposition recorded on ORB-13088). Every rule number from v1 keeps its meaning.
  - Added R23 (append-only migrations, P2), R24 (grandfather or migrate on stricter validation,
    C6), R25 (SHOULD: reconcile shipped defaults by provenance, P6) and R26 (SHOULD: fixes
    reach existing installs and running processes, P7) to the renamed "Version skew and state
    evolution" cluster.
  - Added R27 (pin moving references once, K1) and R28 (guards trigger on actual interference,
    T17) in the new "Moving inputs and integrity guards" cluster.
  - Added R29 (positive proof of ownership before destruction, T4) and R30 (failure paths
    preserve completed work, P1) in the new "Destructive operations and recovery" cluster.
  - Added R31 (lost reply after dispatch is outcome unknown; no TTL reclaim, K4) and R32 (fetch
    before judging, compare-and-set pushes, pinned merges, K6) in the new "Distributed and
    remote effects" cluster.
  - Extended R1 (no lock or transaction across slow work; blocking calls off async threads,
    K2), R5 (one shared write helper; multi-file records staged and renamed, T11), R6
    (read-modify-write under the owning lock or compare-and-set, T11), R10 (declare a change
    incompatible when in doubt, P2), R14 (process identity is PID plus start identity; an
    unanswerable probe is unknown, K5), R15 (bound captured text passed onward, K3) and R22
    (in-process deadlines on every wait; idle deadlines for streams, K3).
  - Filled T10 gaps: R12 now decides survival by probing the group, with an unanswerable probe
    counted as survival; R13 now requires reporting the child's real termination. Added T10's
    uncited evidence to the Why entries for R16 and R19.
  - Clarified R7: the holder record is advisory and can be briefly stale. Added a note to the
    R6 Why that Rust std `File::lock` and `File::try_lock` (stable since 1.89) meet R6 without
    a crate (from the ORB-13117 template review).
  - "Applies to" is now a bullet list of tags with rule scope, deviation examples cite
    `STD-03@2 §Rn`, and `related` adds STD-01, STD-04 and STD-05.
- **v1 (2026-09-26).** First publication: R1 to R22.

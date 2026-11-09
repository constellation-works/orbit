# Changelog — STD-04 Testing and verification

Version history of [STD-04](STD-04.md). Adopting repos pin the `version:` in its
frontmatter as `STD-04@<version>`; a version changes only when a rule is added,
removed or changed in meaning.

- **v2 (2026-10-03):** added R23 (boundary-first testing: integration, golden and e2e by
  default, with unit tests admitted only for combinatorial logic, seam-driven interleavings or
  faults, unreachable security invariants, or platform behaviour), R24 (every golden diff is
  explained by its change) and R25 (retiring a unit test never drops the only guard of a
  security invariant or cited incident). `universal` now covers R1 to R25. Source: Daniel's
  decision of 2026-10-03, the Orbit design pattern `docs/design-patterns/test_strategy.md`
  (constellation-works/orbit#3015) and the unit-test review behind ORB-13812 to ORB-13836.
- **Editorial (2026-09-26, version unchanged):** split into a directory. `STD-04.md`
  keeps the rules, *Applies to* and *Deviations*; *Why*, *Machine checks*,
  *Exemplars* and this changelog moved to `why.md`, `checks.md`, `exemplars.md` and
  `CHANGELOG.md`. No rule changed.
- v1 (2026-09-26): initial, sources ORB-13088.

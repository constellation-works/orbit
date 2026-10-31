# Unit-test admission audit — 2026-10-12

This snapshot audits every identity in the unit-test baseline at `dd268d0d5a1640b709bcac0d3759b04879f77a14` against the admission criteria and economical-boundary precondition in [the test strategy](../../design-patterns/test_strategy.md). The [per-identity report](unit-test-audit-2026-10-12.json) records source locations and hashes, assertion excerpts, the applied criterion, rationale, and a follow-up for every non-admitted identity. The same report is attached as `unit-test-audit.json` to the audit task.

The baseline lists **975 identities** across 17 crates; **968** still have a source function and **7** are stale allowances. The original brief’s 943 and the pilot’s 949 describe earlier revisions. This audit recommends **313 keep, 38 retire, and 624 move**. These are static design judgments. No tests or baseline allowances are removed in this delivery; each crate has a linked follow-up for applying its decisions.

- **Keep:** a qualifying combinatorial, deterministic fault/interleaving, inaccessible safety, or platform-kernel invariant with a concrete reason that public proof would cost more. Required isolated child entry points stay with their admitted parent fixture.
- **Retire:** remove stale allowances, policy/getter/wiring/benchmark cases, or consolidate existing duplicate controls into the specified invariant. Preserve behavior that remains useful before removing its old location.
- **Move:** relocate existing cases to crate-root tests through the public API, built CLI, HTTP route, or served browser assets. This authorizes no new test behavior and no production visibility widening solely for tests. Where existing boundary coverage already proves the exact guarantee, the follow-up can retire the duplicate with evidence.

A public helper is itself an integration boundary. The audit therefore recommends moving some previously documented exceptions whose rationale compares only with a full CLI/provider flow: provider timeout derivation, scheduler arithmetic/DST and validation evidence, guide injection, and candidate-header parsing. The current exports permit the same proof directly with equivalent inputs. The follow-ups must reconcile the affected admission explanations when they relocate those tests. Other documented private snapshot readers, transition hooks, and fault seams remain admitted.

| Crate | Keep | Retire | Move | Follow-up |
|---|---:|---:|---:|---|
| orbit-agent | 0 | 1 | 19 | ORB-15838 |
| orbit-automation | 5 | 0 | 31 | ORB-15839 |
| orbit-cli | 15 | 4 | 10 | ORB-15840 |
| orbit-cmd | 15 | 3 | 22 | ORB-15841 |
| orbit-common | 18 | 0 | 32 | ORB-15842 |
| orbit-config | 3 | 3 | 24 | ORB-15843 |
| orbit-core | 142 | 10 | 198 | ORB-15844 |
| orbit-engine | 29 | 7 | 78 | ORB-15845 |
| orbit-exec | 18 | 2 | 39 | ORB-15846 |
| orbit-mcp | 8 | 1 | 16 | ORB-15847 |
| orbit-policy | 0 | 0 | 12 | ORB-15848 |
| orbit-registry | 2 | 0 | 9 | ORB-15849 |
| orbit-search | 0 | 0 | 2 | ORB-15850 |
| orbit-store | 28 | 4 | 40 | ORB-15851 |
| orbit-tools | 6 | 0 | 18 | ORB-15852 |
| orbit-types | 0 | 3 | 40 | ORB-15853 |
| orbit-web | 24 | 0 | 34 | ORB-15854 |

The seven absent identities are four state-tool tests in orbit-core and three active-run-path tests in orbit-store. Their follow-ups remove the stale entries without inventing replacement tests. The baseline intentionally still names them until those follow-ups apply.

Follow-up implementers must recheck source drift and existing integration coverage, preserve retained harness dependencies, remove unused test modules/support, regenerate only the affected baseline entries while preserving concurrent admissions, and run the required workspace gates. The audit does not change production behavior, authorize dispatch of the follow-ups, or claim that their relocations have already landed.

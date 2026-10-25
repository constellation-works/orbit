---
title: Operations as Data — Vision
owner: claude
last_updated: 2026-10-20
last_validated: 2026-10-25
status: Accepted
feature: operations-as-data
doc_role: vision
type: design
summary: Questions kernel v2 leaves open — typed requests and responses, derived routes, the knowledge/execution split — and what the registry could enable beyond deduplication; ID routing was answered by phase 3a.
tags: [operations-as-data, architecture, adr-0209]
paths: ["crates/orbit-common/src/governance/operation/**"]
related_features: [operations-as-data, orbit-core]
related_artifacts: [ORB-10358, ORB-15937]
---

# Operations as Data — Vision

Forward-looking only. Everything below is a question or an option, not a plan.
The friction pilot and the structural half of kernel v2 (effect, noun spec,
redaction class, generic adapters) are built; input validation and the
migration order are accepted and implemented in [2_design.md](./2_design.md), which
records what this doc used to leave open:

- *Which nouns migrate next, and when?* Answered by
  [2_design.md §9](./2_design.md#9-migration-plan): the completed scoped migration, starting
  with auto-task, instead of the touch-it-move-it ratchet.
- *Do nouns with side-effecting verbs fit `OperationSpec` unchanged?* Answered
  for the verb-level case by `effect` ([2_design.md §5](./2_design.md#5-effect-derived-runtime-wiring));
  input-dependent effects stay in handlers.
- *Should cross-field validation be declared?* Answered for the common case by
  `at_least_one_of` ([2_design.md §4](./2_design.md#4-input-validation-at-the-tool-entry));
  richer rules stay in handlers.

## 1. Open Questions

1. **Do responses become data too?** A spec declares the request but not the
   response shape. A declared response schema would let the CLI table columns,
   the dashboard's TypeScript types, and an MCP output schema derive from one
   place, at the cost of a second, larger wall of contract strings. Now that
   several nouns besides friction have migrated, compare their renderers before
   deciding whether declared response schemas are worthwhile.
2. **Should requests become typed structs?** The friction pilot kept
   `serde_json::Value` partly because handlers accepted undeclared aliases.
   Kernel v2 declares aliases and validates input at the tool entry, so a
   per-verb `#[derive(Deserialize)]` request is now possible. It would give
   handlers compile-time field access, but every parameter would be declared
   twice (spec and struct) unless one is generated from the other, which brings
   back the codegen question the pilot avoided.
3. **Should dashboard routes be derivable at all?** [2_design.md §8](./2_design.md#8-dashboard-adapter)
   argues a REST path is an interface design choice. A weaker version — a
   *default* route shape per effect (GET for `Read`, POST for `Write`),
   overridable per noun — might capture most of the value without pretending
   HTTP is generated. Untested.
4. **Is ID-addressed routing a spec field?** *Answered by phase 3a
   ([ORB-15937]): no, for now.* ID routing stays keyed by tool name in the MCP
   rule tables (`ID_ROUTED_TASK_TOOLS`, `NOT_ID_ROUTED_TOOLS`) and
   `ID_RESOLVED_WORKSPACE_TOOLS`, and the migrated task verbs' CLI derives its
   owner binding and host preflight from them; see
   [ID routing is keyed by tool name in the MCP rule tables, not declared in the spec](./4_decisions.md#id-routing-is-keyed-by-tool-name-in-the-mcp-rule-tables-not-declared-in-the-spec).
   Run IDs are host-local and friction IDs workspace-local, so neither routes.
   The question reopens when the last task-ID tool has a spec.
5. **Does the split table survive bearing 2?** Bearing 2 of the north-star
   decision (knowledge/execution split) would move the knowledge nouns' handlers
   away from the current `OrbitRuntime`. If handlers end up in a crate that can
   sit below the surfaces, the spec and handler halves could merge — the shape
   bearing 1 originally described.
6. **Is per-noun verb enum the right granularity?** A single global verb enum
   would allow one registry and one dispatch, but would put every noun's verbs in
   one file and lose the per-noun exhaustiveness that makes a missing surface
   table entry a local error. Kernel v2's generic tool and CLI impls remove most
   of the per-noun boilerplate that made a global enum attractive.
7. **Can placement and permission share a declaration after all?**
   `GOVERNED_OPERATIONS` is keyed by tool name and also covers CLI commands that
   have no tool, so it cannot fold into the spec today. If every governed
   operation becomes a migrated verb, a `governed: Option<&[McpCapability]>`
   field could replace most of the table, and checking that placement and
   permission pair as intended would compare two fields of one struct instead of
   two tables.

## 2. Prior Work

### Inside Orbit

- **[North-star architecture bearing: operations as data behind an operation registry](../orbit-core/4_decisions.md#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry)**
  — the bearing this feature implements, with the pilot outcome.
- **`crates/orbit-cli/src/command/operation/registry.rs`** — the "commands as
  data" table for top-level CLI dispatch (runtime need, audit metadata, JSON
  error preference). Same instinct, different axis: it declares cross-cutting
  policy per top-level command, where this feature declares the operation. Kernel
  v2 makes migrated arms one generic helper call.
- **`orbit_common::governance::authorization::GOVERNED_OPERATIONS`** — the
  permission registry, built in the same leaf crate for the same layering
  reason; see question 7.
- **[Extract the CLI-facing command layer into orbit-cmd](../orbit-core/4_decisions.md#extract-the-cli-facing-command-layer-into-orbit-cmd)**
  — the extraction whose residuals (runtime-entangled command groups held back
  by inherent impls and the orphan rule) are the pain bearing 1 exists to
  dissolve.

### Outside Orbit

- Command-pattern registries in CLI frameworks that build parsers from
  declarative tables rather than derive macros.
- Interface-definition languages (protobuf/gRPC, OpenAPI) as the maximal version
  of this idea: one schema, generated surfaces. Orbit's version stays in-language
  and hand-written on purpose; the pilot's premise is that a codegen step costs
  more than the duplication it removes at this scale.

## 3. What May Be Distinctive

The split table joined by a typed verb enum gets compile-time exhaustiveness
across a crate boundary without a build step, a trait object, or a runtime
registration phase. The leaf crate owns the declaration, the upper crates own
behaviour, and the enum makes "declared but not handled" impossible to ship.
Kernel v2 generalizes it: every surface that needs per-verb behaviour keeps its
own exhaustive table, so the trick applies on every surface instead of only
between the spec and the handler.

The second distinctive choice is what stays undeclared. Renderers, routes and
schema extensions are presentation and interface design, not properties of the
operation. A registry that swallowed them would be bigger and would encode
decisions that deserve to be made case by case.

## 4. References

### Orbit-internal

- [1_overview.md](./1_overview.md) — what this feature is and its status.
- [2_design.md](./2_design.md) — what exists, kernel v2, and the migration plan.
- [references/cookbook.md](./references/cookbook.md) — how to migrate a noun.
- [ARCHITECTURE.md](../../../ARCHITECTURE.md) — crate layering that constrains
  where the spec table can live.
- [docs/design/mcp-bridge/references/conformance-v1.yaml](../mcp-bridge/references/conformance-v1.yaml)
  — the MCP exposure contract the registry must keep reproducing.

### External

- clap's derive-versus-builder equivalence, which the CLI adapter depends on for
  byte-stable `--help`.

## Task References

- [ORB-10358] — the friction pilot that produced the questions above.
- [ORB-15937] — phase 3a, which answered question 4.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

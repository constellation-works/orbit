---
title: Operations as Data — Vision
owner: claude
last_updated: 2026-07-26
last_validated: 2026-09-23
status: Accepted
feature: operations-as-data
doc_role: vision
type: design
summary: Open questions left by the friction pilot — which nouns migrate next, whether responses become data, and what the registry could enable beyond deduplication.
tags: [operations-as-data, architecture, adr-0209]
paths: ["crates/orbit-common/src/governance/operation.rs"]
related_features: [operations-as-data, orbit-core]
related_artifacts: [ORB-10358]
---

# Operations as Data — Vision

Forward-looking only. Everything below is a question or an option, not a plan;
the friction pilot is the only thing that has actually been built. Sequencing is
governed by the touch-it-move-it ratchet, so this doc deliberately does not
schedule migrations.

## 1. Open Questions

1. **Do responses become data too?** Today a spec declares *which* rendering a
   verb wants (`CliRender`) but not the response shape. A declared response
   schema would let the CLI table columns, the dashboard's TypeScript types, and
   the MCP output schema derive from one place — at the cost of a second, larger
   wall of contract strings. Unclear whether the payoff exceeds the friction
   pilot's ratio.
2. **Should dashboard routes be derivable at all?** §5 of [2_design.md](2_design.md)
   argues a REST path is an interface design choice. A weaker version — declaring
   a *default* route shape per verb kind, overridable per noun — might capture
   most of the value without pretending HTTP is generated. Untested.
3. **What happens to nouns whose verbs are not uniform?** Friction's seven verbs
   are all "one JSON in, one JSON out." Tasks have verbs with side effects on
   lifecycle state, reservations, and the semantic index. Whether those fit
   `OperationSpec` unchanged, or need an effects declaration, is unknown until
   someone tries.
4. **Does the split table survive bearing 2?** [North-star architecture bearing: operations as data behind an operation registry](../orbit-core/4_decisions.md#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry) bearing 2 (knowledge /
   execution split) would move the knowledge nouns' handlers away from the
   current `OrbitRuntime`. If handlers end up in a crate that can sit below the
   surfaces, the spec and handler halves could merge — which is the shape bearing
   1 originally described.
5. **Is per-noun verb enum the right granularity?** A single global verb enum
   would allow one registry and one dispatch, but would put every noun's verbs in
   one file and lose the per-noun compile-time exhaustiveness that makes a
   missing handler a local error.
6. **Cross-field validation.** Rules like "update needs at least one of status,
   tags, body" live in handlers and are invisible to every surface, so the CLI
   cannot pre-reject them and MCP cannot advertise them. Worth declaring, or
   worth leaving as domain logic?

## 2. Prior Work

### Inside Orbit

- **[North-star architecture bearing: operations as data behind an operation registry](../orbit-core/4_decisions.md#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry)** — the north-star bearing this feature implements, now carrying the
  pilot outcome and the ratchet.
- **`crates/orbit-cli/src/command/operation_registry.rs`** — the pre-existing "commands as data"
  table for *top-level CLI dispatch* (runtime need, audit metadata, JSON error
  preference). Same instinct, different axis: it declares cross-cutting policy
  per top-level command, where this feature declares the operation itself. The
  two compose — the friction arm of that table now reads the operation registry.
- **[Extract the CLI-facing command layer into orbit-cmd](../orbit-core/4_decisions.md#extract-the-cli-facing-command-layer-into-orbit-cmd) / ORB-10016** — the orbit-cmd extraction whose documented residuals
  (runtime-entangled command groups that could not move because of inherent impls
  and the orphan rule) are the concrete pain bearing 1 exists to dissolve.

### Outside Orbit

- Command-pattern registries in CLI frameworks that build parsers from
  declarative tables rather than derive macros.
- Interface-definition languages (protobuf/gRPC, OpenAPI) as the maximal version
  of this idea: one schema, generated surfaces. Orbit's version stays in-language
  and hand-written on purpose — the pilot's whole premise was that a codegen step
  is a heavier tax than the duplication it removes at this scale.

## 3. What May Be Distinctive

The split table joined by a typed verb enum is the interesting part. It gets
compile-time exhaustiveness across a crate boundary without a build step, a trait
object, or a runtime registration phase: the leaf crate owns the declaration, the
upper crate owns the behavior, and the enum makes "declared but unimplemented"
impossible to ship. Most registry patterns buy that guarantee with either codegen
or runtime lookup; this one buys it with a `match`.

The second distinctive choice is what was *left* undeclared. Renderers and routes
stayed hand-written because they are presentation and interface design, not
properties of the operation. A registry that swallowed them would be bigger and
would encode decisions that deserve to be made case by case.

## 4. References

### Orbit-internal

- [1_overview.md](1_overview.md) — what this feature is.
- [2_design.md](2_design.md) — what exists today, including honest limitations.
- [references/cookbook.md](references/cookbook.md) — how to migrate the next noun.
- [ARCHITECTURE.md](../../../ARCHITECTURE.md) — crate layering that constrains
  where the spec table can live.
- [docs/design/mcp-bridge/references/conformance-v1.yaml](../mcp-bridge/references/conformance-v1.yaml)
  — the MCP exposure contract the registry must keep reproducing.

### External

- clap's derive-vs-builder equivalence, which the CLI adapter depends on for
  byte-stable `--help`.

## Task References

- [ORB-10358] — the friction pilot that produced the open questions above.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

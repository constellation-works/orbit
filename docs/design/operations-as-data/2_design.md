---
title: Operations as Data — Design
owner: claude
last_updated: 2026-09-09
last_validated: 2026-09-09
status: Accepted
feature: operations-as-data
doc_role: design
type: design
summary: How the operation spec kernel, the split spec/handler table, and the MCP and CLI adapters work today on the friction noun.
tags: [operations-as-data, architecture]
paths: ["crates/orbit-common/src/governance/operation.rs", "crates/orbit-common/src/governance/friction/**", "crates/orbit-tools/src/builtin/orbit/operation.rs", "crates/orbit-cli/src/command/operation_args.rs"]
related_features: [operations-as-data, orbit-core]
related_artifacts: []
---

# Operations as Data — Design

What is implemented today, on the friction noun only. Migrating the remaining
nouns is governed by the touch-it-move-it ratchet (§6), not scheduled here.
Forward-looking questions live in [3_vision.md](3_vision.md).

## 1. The kernel

`orbit_common::governance::operation` holds the vocabulary and nothing else — no clap types,
no axum types, no `OrbitRuntime`. That is what lets it sit in the leaf crate
every surface can already read, adding no dependency edge anywhere.

```rust
pub struct OperationSpec<V: 'static> {
    pub verb: V,                       // typed verb, joins spec ↔ handler
    pub name: &'static str,            // "add" — CLI subcommand + audit subcommand
    pub tool_name: &'static str,       // "orbit.friction.add"
    pub tool_description: &'static str,
    pub cli_about: &'static str,
    pub params: &'static [ParamSpec],  // declaration order is contract
    pub rejects_agent_field: bool,
    pub mcp_scope: Option<McpToolScope>,
    pub cli_json_flag: bool,
    pub cli_render: CliRender,
}
```

A `ParamSpec` carries the wire field name, its `ParamType`, whether it is
required, an **optional** MCP description, and an **optional** CLI binding. Both
sides are optional independently, so a parameter can be MCP-only, CLI-only, or
both. The CLI binding carries its own help text because MCP and CLI wording
legitimately differ for the same field — `show`'s `id` is `friction ID` over MCP
and `Friction record id, e.g. F2026-05-001` on the command line, and forcing
those to converge would have been a wire change.

`Description` is either `Static(&'static str)` or `Computed(fn() -> String)`.
The computed form exists because some descriptions interpolate live
configuration (the friction taxonomy list), and both forms are
const-constructible, so a whole registry stays a `const` item.

## 2. The split table

One literal table is not practical: handlers need `&OrbitRuntime`, which lives
far above `orbit-common`. Co-locating them would either drag the runtime into the
leaf crate or push the specs above the surfaces that must read them.

The shape that works is **one table split across two crates, joined by the verb
enum**:

| Half | Lives in | Form |
|------|----------|------|
| Spec | `orbit-common` | `&'static [OperationSpec<V>]` |
| Handler | `orbit-core` | one exhaustive `match` on `V` |

`FrictionVerb::spec()` is likewise an exhaustive `match`. Adding a verb therefore
breaks compilation in exactly two places — the spec lookup and the handler
table — until it is fully wired. A spec with no handler cannot ship.

## 3. MCP adapter

`orbit_tools::builtin::orbit::operation` is two generic functions:
`operation_tool_schema(spec)` projects the spec's MCP parameters into a
`ToolSchema` in declaration order, and `register_operation(registry, spec, tool)`
registers the tool when `mcp_scope` is `Some`, or keeps it off MCP when the value
is `None`. The scope says only whether dispatch is global or requires a workspace;
it carries no placement or capability policy.

`FrictionOperationTool(&'static FrictionOperation)` is the single `Tool` impl
that replaced seven. Its `execute` applies `rejects_agent_field` (only
`orbit.friction.add` sets it — attribution is `model`-only) and dispatches
`OrbitBuiltinAction::Friction(spec.verb)`. Registration is a `for` loop over the
registry; `register_builtins` mentions no friction verb by name.

`OrbitBuiltinAction` collapsed its seven `Friction*` variants into one
`Friction(FrictionVerb)`. Downstream matches on specific verbs — artifact
redaction policy for `Add`/`Update`, for example — pattern-match on the inner
verb. Core's handler match remains exhaustive, so a new verb cannot omit runtime
wiring.

The `list` spec also declares the MCP-only `response_mode` parameter. Omitting
it preserves the legacy record-array response for matches and every empty
result. The explicit `with_notes` mode has one stable object shape,
`{records, notes}`, so callers can request search guidance without making the
default response conditional. The tool description and parameter description
advertise both alternatives because the generic tool schema has no output
schema field.

## 4. CLI adapter

`orbit_cli::command::operation_args` builds a `clap::Command` subcommand per
registry entry and projects parsed matches back into the tool input. It is
generic over `V`; `command/friction.rs` supplies the registry and keeps only the
`Subcommand`/`FromArgMatches` impls and the response renderers.

Byte-for-byte help compatibility is the load-bearing requirement, and it holds
because the adapter reproduces what `#[derive(Args)]` generates:

- arg id is the wire field name (`during_task`), the flag spelling comes from the
  binding (`--during-task`), and they are allowed to differ (`tags` ↔ `--tag`);
- value name is the field name in SCREAMING_SNAKE (`<DURING_TASK>`);
- args are added in declaration order, and clap assigns display order from
  insertion order for non-positional args — which is why parameter order in the
  spec is contract, not style;
- `Vec`-shaped params use `ArgAction::Append` plus the spec's value delimiter.

Input projection has one rule worth stating: **optional** string parameters are
trimmed and dropped when blank, so an unset filter is absent rather than
present-and-empty; **required** parameters pass through verbatim so that
"you passed only whitespace" is reported by the handler, where the domain rules
live. That reproduces the pre-migration behavior exactly.

Audit metadata is derived too. `command/operation.rs`'s friction arm reads
`invocation.spec.name` and `invocation.target_id()` — the latter resolved by
looking up the spec's positional parameter — instead of matching verb by verb.

The friction CLI injects `response_mode: with_notes` for list calls so its
human table can include search guidance. Its machine-readable document projects
the envelope's `records` back to the historical array, keeping `--json`
compatible.

## 5. Dashboard adapter

The dashboard is the partial case, and deliberately so. `FrictionCall` takes tool
names from `FrictionVerb::tool_name()` and validates every field name it sets
against the verb's spec, so a registry rename the dashboard misses surfaces as a
4xx naming the field rather than as a silently dropped filter. What stays
hand-written is genuinely HTTP: the REST route shapes, the serde request bodies,
and dashboard-specific defaults (the `limit` cap, the human-actor fallback for
`model`, the `tag_options` enrichment on GET). A REST path is an interface design
choice, not a property of the verb, so deriving it was rejected rather than
deferred.

The dashboard list adapter explicitly requests `with_notes`, maps `records` to
its `items` field, and forwards non-empty notes for the hint UI.

## 6. The touch-it-move-it ratchet

The next feature that touches a hand-copied noun migrates that noun as part of
that work. "Touches" means adding or removing a verb, changing a verb's
parameters, or changing how any surface wires that noun — not an unrelated edit
inside an existing handler body. Reviewers seeing a new hand-wired adapter for a
migratable noun should ask for the migration or for an explicit reason in the PR.
The step-by-step procedure is [references/cookbook.md](references/cookbook.md).

## 7. Concerns & Honest Limitations

- **The registry is a wall of contract strings.** Centralizing tool names,
  parameter names, flag spellings, and help text makes that one file a
  high-blast-radius edit. That is the intended trade — the strings were always
  contract, they were just scattered — but it is a real change in how a careless
  edit fails.
- **Renderers are not data.** `CliRender` says *which* rendering a verb wants;
  the friction record/table printers still know friction field names. A noun with
  a genuinely novel output shape needs a new `CliRender` variant plus a renderer,
  which is a two-place edit.
- **Dashboard routes are not derived** (§5). Adding a friction verb that should
  be reachable over HTTP still requires a route.
- **`value_name` interning leaks.** `clap::Arg::value_name` accepts only
  `&'static str`, so the adapter `Box::leak`s the SCREAMING_SNAKE form. The
  command tree is built once per process over fixed-size `&'static` tables, so
  the leak is bounded and small — but it is a leak, and tests that rebuild
  commands in a loop pay it repeatedly.
- **Help stability is a convention, not a type.** The adapter matches
  `#[derive(Args)]`'s conventions because it was written to; nothing in the type
  system enforces that a future clap upgrade keeps them aligned. The frozen
  `friction_help/*.txt` fixtures are the actual guard, and every future noun
  migration must capture its own before starting.
- **Only one noun is migrated.** Readers of `orbit-tools` and `orbit-cli` will
  meet both the registry-driven and hand-wired shapes until the ratchet moves
  more nouns.
- **Verb-level parameter validation is still the handler's job.** The spec
  declares types and requiredness; cross-field rules (`update` needs at least one
  of `status`/`tags`/`body`) live in the handler and are not visible to any
  surface.
- **There are no per-verb Rust request/response structs.** What shipped is a
  declaration of the request (`params`) with `serde_json::Value` still on the
  wire. Derived request structs would complicate the documented field aliases
  (`body`/`description`, `tags`/`tag`, `during_task`/`task_id`) that agents send
  today. Typed responses are similarly absent: the response is the store's
  existing JSON projection, and re-typing it would create a second contract to
  keep in sync. Revisit if alias tolerance is retired.

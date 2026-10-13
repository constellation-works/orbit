---
title: Operations as Data — Design
owner: claude
last_updated: 2026-10-12
last_validated: 2026-10-12
status: Accepted
feature: operations-as-data
doc_role: design
type: design
summary: The operation spec kernel v2 (noun spec, effect, redaction class, parameter groups, generic tool and CLI adapters) as built on the friction noun, plus the planned input validation, aliases and richer CLI parameters and the planned migration of the remaining nouns.
tags: [operations-as-data, architecture]
paths: ["crates/orbit-common/src/governance/operation.rs", "crates/orbit-common/src/governance/friction/**", "crates/orbit-tools/src/builtin/orbit/operation.rs", "crates/orbit-cli/src/command/operation/args.rs", "crates/orbit-cli/src/command/operation/spec.rs", "crates/orbit-cli/src/command/friction.rs", "crates/orbit-core/src/adapter/tool_host/artifact_redaction.rs", "crates/orbit-core/src/adapter/tool_host/worker_tools.rs", "crates/orbit-common/src/protocol/tool_input.rs"]
related_features: [operations-as-data, orbit-core]
related_artifacts: [ORB-10358, ORB-15932]
---

# Operations as Data — Design

This doc describes the friction implementation and the revisions accepted on
2026-10-12 for every noun that migrates after it. Each section says whether it
is **Implemented** (true of the code today, on friction only) or **Planned**
(accepted but not yet built). Kernel v2's structure is implemented; its input
validation and richer parameter kinds are planned. Both follow the rules in
[4_decisions.md](./4_decisions.md). As a phase lands, its sections move from
Planned to Implemented. The order in which nouns migrate is in §9;
forward-looking questions are in
[3_vision.md](./3_vision.md).

## 1. Where a fact lives

**Implemented** for friction; the rule binds every noun that migrates.

The friction pilot put every fact it could into the spec, including the CLI's
rendering choice. Facts it could not express as data stayed behind on several
surfaces, sometimes in more than one place. Kernel v2 sorts every fact about a
verb into exactly one of two homes:

| Kind of fact | Home | Examples |
|---|---|---|
| Needed by two or more surfaces, or by the single tool entry every surface traverses | The spec, in `orbit-common` | names, parameters, effect, redaction class, MCP placement; later aliases and cross-field presence rules |
| Needed by one surface only | An exhaustive `match` on the verb enum, in that surface's crate | handlers (`orbit-core`), CLI renderers and CLI-only request shaping (`orbit-cli`), claimed-worker routing (`orbit-core`); later MCP schema extensions (`orbit-tools`) |

The second home keeps the property that makes the split table work: adding a
verb fails to compile in every surface table that has not decided what to do
with it. A wildcard arm (`_ =>`) in a surface table defeats that and is not
allowed for a migrated noun. Friction's tables in `friction_tools.rs`,
`worker_tools.rs`, `artifact_redaction.rs` and `command/friction.rs` follow it.
See
[The spec holds cross-surface facts; single-surface facts are an exhaustive match in that surface](./4_decisions.md#the-spec-holds-cross-surface-facts-single-surface-facts-are-an-exhaustive-match-in-that-surface).

## 2. The kernel

`orbit_common::governance::operation` holds the vocabulary and nothing else: no
clap types, no axum types, no `OrbitRuntime`. That is what lets it sit in the
leaf crate every surface can already read.

### 2.1 As built (Implemented)

```rust
pub struct NounSpec<V: 'static> {
    pub cli_name: &'static str,             // "friction"; "auto-task" later
    pub tool_prefix: &'static str,          // "orbit.friction"; "orbit.auto_task" later
    pub cli_about: &'static str,            // `orbit friction --help` summary
    pub audit_target_type: &'static str,    // "friction"
    pub cli_groups: &'static [CliGroup],    // intermediate subcommands; friction has none
    pub operations: &'static [OperationSpec<V>],
}

pub struct OperationSpec<V: 'static> {
    pub verb: V,                       // typed verb, joins spec ↔ every surface table
    pub cli_path: &'static [&'static str], // ["add"]; ["artifact", "get"] later
    pub tool_name: &'static str,       // "orbit.friction.add"
    pub tool_description: &'static str,
    pub cli_about: &'static str,
    pub effect: Effect,                // Read | Write
    pub params: &'static [&'static [ParamSpec]], // groups, concatenated in order
    pub rejects_agent_field: bool,
    pub mcp_scope: Option<McpToolScope>,
}

pub struct ParamSpec {
    pub name: &'static str,
    pub param_type: ParamType,         // String | StringList | Integer | Boolean
    pub required: bool,
    pub preserve_empty: bool,
    pub redact: Redaction,             // None | FreeText | Path
    pub mcp_description: Option<Description>,
    pub cli: Option<CliBinding>,
}

pub trait OperationVerb: Copy + Eq + Debug + Send + Sync + 'static {
    fn noun() -> &'static NounSpec<Self>;
    fn spec(self) -> &'static OperationSpec<Self>;
    fn tool_name(self) -> &'static str { /* from spec */ }
}
```

A `ParamSpec` carries the wire field name, its `ParamType`, whether it is
required, whether an empty string is meaningful (`preserve_empty`), its
redaction class, an **optional** MCP description, and an **optional** CLI
binding. Both sides are optional independently, so a parameter can be MCP-only,
CLI-only, or both. The CLI binding carries its own help text because MCP and CLI
wording legitimately differ: `show`'s `id` is `friction ID` over MCP and
`Friction record ID, e.g. FYYYY-MM-NNN` on the command line.

`Description` is either `Static(&'static str)` or `Computed(fn() -> String)`.
The computed form exists because some descriptions interpolate live
configuration (the friction taxonomy list). Both forms are const-constructible,
so a whole registry stays a `const` item.

What each kernel v2 piece is for:

- **`NounSpec`** holds the noun-level strings that friction used to restate by
  hand: the audit target type and noun name in the CLI registry arm, and the
  `#[command(about)]` on `FrictionCommand`, which now reads `FRICTION.cli_about`.
  It is what lets one generic CLI impl and one generic tool type serve every
  noun (§6, §7). `cli_name` and `tool_prefix` are separate because `auto-task`
  and `orbit.auto_task` already differ in shipped surfaces.
- **`cli_path`** replaces the old `name` so that nested commands (`task artifact
  get`, `task locks reserve`) can be declared and not hand-wired. The audit
  subcommand label is the path joined with `-`; each migration checks the
  derived labels against the hand-wired arm it replaces, because audit rows are
  queried by them. Friction's paths are all one segment, and its labels are
  unchanged.
- **`effect`** is §5.
- **Parameter groups** (`&[&[ParamSpec]]`) carry flags shared between verbs,
  such as auto-task's schedule flags (`schedule_args.rs`, used by `add` and
  `update`), without functional record update, which `const` does not allow.
  `OperationSpec::params()` iterates every group in order. Friction declares one
  group per verb.
- **`redact`** moves the per-verb field lists in
  [artifact_redaction.rs](../../../crates/orbit-core/src/adapter/tool_host/artifact_redaction.rs)
  onto the parameters they name, so a new free-text parameter is redacted where
  it is declared. Friction's `add` and `update` mark `body` and `title`
  `FreeText`. Two things stay in the core surface table: `add`'s undeclared
  `description` alias for `body`, listed per verb until §2.2's `aliases` land,
  and nested policies such as the auto-task template's `nested_objects`, until
  a second noun needs them.
- **`CliGroup`** names an intermediate subcommand and its `--help` summary. No
  migrated noun declares one yet; the CLI adapter builds groups when the first
  nested noun (task, phase 3) needs them.
- **`OperationVerb`** is what the generic adapters are generic over. The
  `operation_registry!` macro (§3) implements it.

`cli_render` left the spec: rendering is a single-surface fact (§7).

### 2.2 Kernel v2 input and parameter kinds (Planned)

```rust
pub struct OperationSpec<V: 'static> {
    // … as built, plus:
    pub at_least_one_of: &'static [&'static str],
    pub retired_fields: &'static [RetiredField], // name + where it went
}

pub struct ParamSpec {
    // … as built, plus:
    pub param_type: ParamType,              // + Enum(&'static [&'static str]), Switch, Object
    pub aliases: &'static [&'static str],   // accepted wire spellings, resolved at the tool entry
    pub default: Option<&'static str>,
    pub cli: Option<CliBinding>,            // CliArgKind::Flag gains `aliases` and `short`
}
```

What each addition is for:

- **`at_least_one_of`** declares the one cross-field rule that recurs:
  friction `update` refuses an edit that changes nothing, and today the CLI
  restates that rule by hand (`require_update_field` in
  [friction.rs](../../../crates/orbit-cli/src/command/friction.rs), called from
  the CLI's `prepare` table). Richer cross-field rules stay in handlers.
- **`retired_fields`** generalizes both `rejects_agent_field` and
  `reject_retired_task_add_input_fields`: a field that used to be accepted is
  refused with a message that names where it went.
- **`aliases`** declares the alternative spellings handlers already accept
  silently: `description` for `body`, `task_id` for `during_task`, `tag` for
  `tags`. §4 uses them, and redaction then derives an alias's class from its
  parameter.
- **`default`**, **`Enum`**, **`Switch`**, and flag `aliases`/`short` are the
  clap features the task and auto-task commands use and the friction kernel
  cannot express: `value_enum` with `default_value_t`, presence flags such as
  `--allow-missing-context`, and `alias = "dependency"`. `Object` is a JSON
  value, for structured inputs such as task relations that are MCP-only today.

Friction's registry was the first thing ported to v2, with no change to its
wire surface: the friction help goldens and the MCP snapshot are byte-identical
across the port, so they checked the new kernel before any new noun relies on
it.

## 3. The split table

**Implemented.**

One literal table is not practical: handlers need `&OrbitRuntime`, which lives
far above `orbit-common`. Co-locating them would either drag the runtime into the
leaf crate or push the specs above the surfaces that must read them. The shape
that works is **one table split across two crates, joined by the verb enum**:

| Half | Lives in | Form |
|------|----------|------|
| Spec | `orbit-common` | `NounSpec<V>` holding `&'static [OperationSpec<V>]` |
| Handler | `orbit-core` | one exhaustive `match` on `V` |

`FrictionVerb::spec()` is also an exhaustive `match`. Adding a verb therefore
breaks compilation in the spec lookup and in the handler table until it is
fully wired; under §1 it also breaks every other surface table for that noun.
See [Split spec/handler table joined by a typed verb enum](./4_decisions.md#split-spechandler-table-joined-by-a-typed-verb-enum).

The verb enum, its `spec()` match and the ordered operations list must agree.
`operation_registry!` emits all three, with the `OperationVerb` impl, from one
`Variant => SPEC` list, so the invariant holds by construction: list order is
enum order and `--help` order. The noun's `NounSpec` const sits beside it.

## 4. Input validation at the tool entry

**Planned.**

Today the spec describes a verb's input, but nothing checks the input against
the spec. Each handler re-reads it by string name
(`required_string(&input, &["body", "description"], "body")` in
[friction_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/friction_tools.rs)),
which restates every parameter name a fifth time and keeps the accepted aliases
out of the spec. Unknown fields are refused only where a tool calls
`reject_unknown_tool_fields` itself; for friction they are ignored. The
dashboard compensates with its own name check (`FrictionCall::set`).

Kernel v2 adds `OperationSpec::parse_input(&self, input: Value) ->
Result<OperationInput, OrbitError>` in `orbit-common`, built from the existing
helpers in [tool_input.rs](../../../crates/orbit-common/src/protocol/tool_input.rs).
In order, it:

1. requires a JSON object;
2. resolves each declared alias to its canonical name, refusing input that sets
   both;
3. refuses retired fields with their hint, then unknown fields with the existing
   did-you-mean suggestion;
4. checks each present value against its `ParamType` (`StringList` keeps
   accepting a string, an array, or a comma-separated string);
5. checks that required parameters are present and that `at_least_one_of` is
   met.

It checks presence and shape only. Content rules, such as refusing a
whitespace-only required value or validating a status name, stay in the handler
where the domain rules live; that preserves the trimming behaviour the friction
migration was careful to keep.

The generic operation tool (§6) calls `parse_input` before dispatch. Every
surface reaches a tool through `Tool::execute`: MCP, `orbit tool run`, the CLI's
`run_tool`, the dashboard, and the deterministic dispatcher. So one call site
gives every surface the same validation. Handlers then read canonical names from
`OperationInput` and never see an alias.

Refusing unknown fields is a wire change for any caller that sends extra fields
today. Each noun turns it on in two steps: first a `warn!` and an audit row per
unknown field, then refusal after the 30-day compatibility window. See
[Declare effect, aliases, and redaction in the spec, and validate input once at the tool entry](./4_decisions.md#declare-effect-aliases-and-redaction-in-the-spec-and-validate-input-once-at-the-tool-entry).

## 5. Effect-derived runtime wiring

**Implemented** for the tool execution kind and the CLI runtime need.

Before kernel v2, whether a friction verb reads or writes was declared four
times:

| Site | What it said |
|---|---|
| `FrictionOperationTool::execution_kind` (orbit-tools, now deleted) | `list`, `show`, `stats`, `tags` are read-only; everything else (wildcard) is mutating |
| `Commands::Friction` arm ([registry.rs](../../../crates/orbit-cli/src/command/operation/registry.rs)) | only `list` opens the read-only runtime; every other verb opens the full runtime |
| Redaction policy ([artifact_redaction.rs](../../../crates/orbit-core/src/adapter/tool_host/artifact_redaction.rs)) | `add` and `update` redact free text; the rest do not |
| Claimed-worker routing ([worker_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/worker_tools.rs)) | the four reads pass; `add` is rewritten as a claim mutation; other writes are refused |

The first two disagreed: `show`, `stats` and `tags` were read-only tools, but
the CLI opened the full runtime for them, including stale-run reconciliation on
open. Nothing recorded whether that was deliberate.

Each spec now declares `effect: Effect::{Read, Write}`, and two surfaces derive
from it:

- `OperationTool<V>::execution_kind` (§6): `Read` → `ReadOnly`, `Write` →
  `Mutating`;
- `registry_operation`, the generic CLI audit arm (§7): `Read` →
  `RuntimeNeed::ReadOnly`, `Write` → `RuntimeNeed::Required`.

Friction declares `list`, `show`, `stats` and `tags` as `Read`, and `add`,
`update`, `resolve` and `rehome` as `Write`. So `orbit friction show`, `stats`
and `tags` now open the read-only runtime, as `list` already did. None of them
relies on reconciliation on open. That reconciliation only finalizes orphaned
job runs: it marks them interrupted, releases their reservations and records the
interruption on coupled tasks. `show` reads one friction record and `tags` reads
the workspace's tag taxonomy file, neither of which reconciliation touches.
`stats` also reads task metadata, but counts only `done` and `archived` tasks by
implementer, and an interruption never moves a task into either status. Its
task read applies the same replica gate (`coordination_task_reads_visible`)
under both runtimes, because the read-only open still binds the checkout's
replica owner.

`effect` gives the default, not the whole answer. The hand-wired task arm shows
what else the CLI's runtime choice depends on: `task show` and `task artifact
get` route by task ID to the owning workspace (`RuntimeNeed::TaskOwner`), and
`task lint` needs the full runtime only with `--restore-pruned`. ID-addressed
routing is also how `orbit.task.show` resolves over MCP, so it is a
cross-surface fact; phase 3 (§9) decides whether it becomes a spec field or
stays a CLI surface-table override. Input-dependent choices such as
`--restore-pruned` stay in the CLI surface table.

Redaction derives from the parameters' `redact` class (§2.1), not from the
effect. Claimed-worker routing stays a core surface table, an exhaustive `match`
on `FrictionVerb` in `worker_tools.rs`, because its write side carries real
logic (the `add` → claim rewrite) and its read side is one arm beside it.
Input-dependent guards also stay in the handler: friction `update` needs
coordination-write permission only when it moves a record, so it cannot be a
per-verb flag.

## 6. MCP adapter

**Implemented:** `orbit_tools::builtin::orbit::operation` holds the whole MCP
adapter. `OperationTool<V>(&'static OperationSpec<V>)` is the one `Tool` impl
every registry operation is served by, for any `V: OperationVerb` with
`From<V> for OrbitBuiltinAction`. Its schema projects the spec's MCP parameters
into a `ToolSchema` in declaration order; its `execution_kind` comes from
`effect` (§5); `execute` refuses a legacy `agent` field when the spec says so
and dispatches the verb's action. `register_noun(registry, &NOUN)` registers
each operation of a noun at its `mcp_scope`, or keeps it off MCP when the value
is `None`. The scope says only whether dispatch is global or requires a
workspace; it carries no placement or capability policy beyond that.

`OrbitBuiltinAction` collapsed friction's seven `Friction*` variants into one
`Friction(FrictionVerb)`, and `orbit-tools` implements `From<FrictionVerb>` for
it. Downstream matches pattern-match on the inner verb. Friction needs no
module of its own in `orbit-tools`.

The `list` spec also declares the tool-only `response_mode` parameter (no CLI
flag; `list` is not advertised over MCP, where agents list frictions with
`orbit.search`). Omitting it preserves the legacy record-array response. The
explicit `with_notes` mode has one stable object shape, `{records, notes}`.

**Planned:**

- **Validate at the tool entry.** `OperationTool::execute` calls `parse_input`
  (§4) before dispatch.
- **Schema extensions as a surface table.** `orbit.task.add` and
  `orbit.task.update` override `Tool::input_schema()` to add JSON Schema
  conditionals for guarded creation
  ([guarded.rs](../../../crates/orbit-tools/src/builtin/orbit/task/guarded.rs)).
  The flat parameter list cannot express those, and only MCP needs them. Each
  noun's `orbit-tools` module provides an exhaustive `fn schema_extension(verb)
  -> Option<fn(&mut Value)>`, applied after the derived schema.
- **Enum and default in the schema.** `ToolParam` has no `enum` or `default`
  field today. A migrated `Enum` parameter either keeps its allowed values in
  the description (wire-identical), or the schema composer learns to emit
  `enum`/`default` (an additive wire change that re-blesses the MCP snapshot).
  The first noun to need it decides, in its PR.

## 7. CLI adapter

**Implemented:** `orbit_cli::command::operation::args` builds a `clap::Command`
subcommand per registry entry and projects parsed matches back into the tool
input. It implements `Subcommand` and `FromArgMatches` once, for
`Invocation<V>` over every `V: OperationVerb`, reading the operations and noun
name from `V::noun()`; a migrated noun writes no clap impls. The noun's parent
struct holds `#[command(subcommand)] command: Invocation<FrictionVerb>`.

Byte-for-byte help compatibility is the load-bearing requirement. It holds
because the adapter reproduces what `#[derive(Args)]` generates:

- the arg id is the wire field name (`during_task`); the flag spelling comes from
  the binding (`--during-task`), and they may differ (`tags` ↔ `--tag`);
- the value name is the field name in SCREAMING_SNAKE (`<DURING_TASK>`), passed
  to clap as an owned `String` through the workspace's `string` feature, so
  nothing is interned or leaked;
- args are added in declaration order, and clap assigns display order from
  insertion order for non-positional args, which is why parameter order in the
  spec is contract;
- `Vec`-shaped params use `ArgAction::Append` plus the spec's value delimiter.

Input projection trims **optional** string parameters and drops blank values
unless the parameter declares `preserve_empty`. **Required** parameters pass
through verbatim so the handler reports "you passed only whitespace". The
friction CLI executes through `runtime.run_tool`, so the CLI and MCP reach the
same handler through the same capability chokepoint.

[friction.rs](../../../crates/orbit-cli/src/command/friction.rs) keeps only
friction's two CLI surface tables, both exhaustive matches on `FrictionVerb`:
`prepare(verb, &mut Map)` shapes the request (`list` injects `response_mode`;
`update` refuses an edit that changes nothing, naming CLI flags) and
`render(verb, &Value)` picks the record, table, tag-list or JSON rendering. The
kernel carries no rendering vocabulary.

The `Commands::Friction` arm in
[registry.rs](../../../crates/orbit-cli/src/command/operation/registry.rs)
calls one helper, `registry_operation`, which derives `RuntimeNeed` from
`effect` and `admin_meta` from `cli_name`, `audit_target_type`, `cli_path` and
the positional target. Arms that need more, such as `governed_when` for a
flag-gated destructive command, add it around the helper.

**Planned:**

- **Kernel v2 parameter kinds.** `Enum` → `PossibleValuesParser`, so `--help`
  prints `[possible values: …]` as `value_enum` does; `Switch` →
  `ArgAction::SetTrue`, sending `true` only when present; `default` →
  `default_value`, rendering `[default: …]`; flag `aliases` → `.alias()`
  (hidden, like derive's `alias`); `cli_path` → intermediate subcommands built
  from `NounSpec::cli_groups`.
- **Cross-field errors named in CLI terms.** When `parse_input` refuses an
  `at_least_one_of` violation, the CLI maps each field to its flag spelling from
  the spec's bindings (`tags` → `--tag`). `require_update_field` and its
  hand-maintained `UPDATE_FIELD_KEYS` list are deleted.
- **The CLI always goes through the tool.** Every migrated verb executes by
  `run_tool`. For nouns whose CLI calls core services directly today (task,
  auto-task, routine, workflow run) this is a behaviour change, not a refactor:
  capability resolution, audit rows, attribution (`mutation_identity` versus the
  `model` parameter) and write screening (`TaskWriteScreen::from_process`) all
  move to the tool path. See
  [Migrated CLI verbs execute through the tool path](./4_decisions.md#migrated-cli-verbs-execute-through-the-tool-path).

## 8. Dashboard adapter

**Implemented.** The dashboard is the partial case, and deliberately so.
`FrictionCall` takes tool names from `FrictionVerb::tool_name()` and validates
every field name it sets against the verb's spec, so a registry rename the
dashboard misses surfaces as a 4xx naming the field rather than as a silently
dropped filter. What stays hand-written is HTTP: the REST route shapes, the serde
request bodies, and dashboard defaults (the `limit` cap, the human-actor
fallback for `model`, the `tag_options` enrichment on GET). A REST path is an
interface design choice, not a property of the verb; see
[Renderers and HTTP routes stay hand-written](./4_decisions.md#renderers-and-http-routes-stay-hand-written).

The dashboard list adapter explicitly requests `with_notes`, maps `records` to
its `items` field, and forwards non-empty notes for the hint UI.

**Planned:** once §4 lands, the tool entry refuses an undeclared field for every
caller, and `FrictionCall::set`'s own check becomes redundant. It stays as a
thin builder; it is not copied to other nouns.

## 9. Migration plan

**Planned**, except phase 0a, which is implemented. This replaces the
touch-it-move-it ratchet; see
[Migrate the remaining nouns on a plan, not on contact](./4_decisions.md#migrate-the-remaining-nouns-on-a-plan-not-on-contact).

Each phase is one Orbit task tagged `operations-as-data` (large nouns split per
slice); the task IDs follow the table. Each starts with cookbook Step 0 and puts its behaviour diff in the PR.

| Phase | Scope | Why here |
|---|---|---|
| 0a (done) | Kernel v2 structure, ported onto friction: noun spec, `cli_path`, `effect`, parameter groups, `redact`, the registry macro, generic tool and CLI adapters, CLI surface tables | Proves v2 against existing goldens with no new noun at stake; the only behaviour change is §5's read-only runtime for three verbs |
| 0b | Kernel v2 input validation on friction: `parse_input`, aliases, retired fields, `at_least_one_of`, unknown-field warnings | Moves the friction handlers onto canonical names and the warn-then-refuse rollout onto the one migrated noun |
| 1 | auto-task: 5 tools (`show` inactive), 10 CLI verbs | Second pilot. First noun whose CLI calls core directly; exercises `Enum`, `default`, parameter groups, CLI-only verbs (`toggle`, `delete`, `restore`, `recover`, `reset` with `mcp_scope: None`) and the tool-path rule |
| 2 | drain (6 tools) and workspace claim (3 inactive tools) | Tool-only, mechanical; collapses nine `OrbitBuiltinAction` variants; drain verbs keep their agent-floor rows in `GOVERNED_OPERATIONS` |
| 3 | task, in slices: reads (`show`, `list`, `eligible`, `lint`); writes (`add`, `update`, with the guarded schema extension); `artifact get`/`put` (nested); `locks` (nested); review (`reconcile_review`, `review_reset`, `reject`); `delete` | Largest noun; slices keep each PR reviewable. `flow`, `export`, `import`, `publication`, `reindex`, `recheck-blocked` and `archive` have no tool and are judged per subcommand, not migrated by default |
| 4 | workflow run: `workflow.ship`, `workflow.run.show/list/resume`, `workflow.auto` | Operator-governed; the authorization matrix golden in `output_goldens.rs` must not move except as intended |
| 5 | Single-verb tools with a second surface: `routine.control` with `orbit routine pause/resume`; `search` with `orbit search` | A one-verb noun with one surface gains nothing; `agent.invoke`, `command.exec` and `pipeline.invoke/wait` migrate only if they gain a surface |

Tasks: phase 0a is [ORB-15932] (kernel v2 structure, done); phase 0b is
[ORB-15933] (input validation, warn mode) and [ORB-15934] (refuse unknown
fields after the window); phase 1
[ORB-15935]; phase 2 [ORB-15936]; phase 3 [ORB-15937] (reads), [ORB-15938]
(add/update), [ORB-15939] (artifact and locks), [ORB-15940] (review and
lifecycle); phase 4 [ORB-15941]; phase 5 [ORB-15942]. [ORB-15943] verifies the
authorization golden that phases 1 and 4 rely on.

Out of scope: the `Desktop*` host actions (a separate host protocol), GitHub and
process tools, and plugin tools, which have their own declaration format.

Progress is measured, not remembered: the count of hand-written `impl Tool for`
under `crates/orbit-tools/src/builtin/orbit/`, and the number of `OrbitBuiltinAction`
variants other than the `Desktop*` host actions and the nouns' single
`Noun(Verb)` variants. On 2026-10-12 those were 41 and 36 (friction is the one collapsed
noun, and the generic `OperationTool<V>` is one of the 41). The weekly code-health sweep reports both numbers. Neither is a merge
gate.

## 10. Concerns & Honest Limitations

- **The registry is a wall of contract strings.** Centralizing tool names,
  parameter names, flag spellings and help text makes one file a
  high-blast-radius edit. That is the intended trade, since the strings were
  always contract and only scattered, but a careless edit now fails differently.
- **The kernel grows until task lands.** Every new `ParamType` or `CliArgKind`
  variant has to be handled in the MCP adapter, the CLI adapter and
  `parse_input`. Kernel v2 is sized to auto-task and task; each phase may still
  add a variant, and should add it only when the noun in hand needs it.
- **Surface tables spread a noun across files.** Under §1, one noun's behaviour
  lives in the spec plus one exhaustive match per surface that needs one. Every
  table is compiler-checked, but a reader still opens several files.
- **Refusing unknown fields is a wire change** (§4). The warn-then-refuse rollout
  costs a 30-day window per noun, during which a mistyped field is logged but
  still ignored.
- **The tool-path rule changes behaviour for task and auto-task** (§7). A
  migration PR for those nouns is not wire-neutral even when its help goldens and
  MCP snapshot are, and needs a behaviour baseline beyond the goldens.
- **Effect is two-valued.** It derives the cases that depend only on the verb.
  Guards that depend on input (friction `update` moving a record) stay in
  handlers and are visible to no surface.
- **Help stability is a convention, not a type.** The adapter matches
  `#[derive(Args)]`'s conventions because it was written to. The frozen
  `help_goldens/<noun>/*.txt` fixtures are the guard, and every noun migration
  captures its own before starting.
- **Dashboard routes are not derived** (§8). Adding a verb that should be
  reachable over HTTP still needs a route, and nothing catches a forgotten one.
- **Two idioms coexist until phase 5.** Readers of `orbit-tools` and `orbit-cli`
  meet both shapes. The plan bounds the period; it does not remove the cost.
- **There are no per-verb Rust request or response structs.** `serde_json::Value`
  stays on the wire, wrapped by `OperationInput` after validation. Declared
  aliases remove the main reason given for not deriving typed requests; whether
  to do so is [3_vision.md](./3_vision.md) question 2.

## Task References

- [ORB-10358] — built the friction kernel, registry and derived adapters
  described as Implemented above.
- [ORB-15932] — phase 0a: built kernel v2's structure and ported friction onto
  it.
- [ORB-15933], [ORB-15934] — phase 0b: input validation in warn mode, then
  unknown-field refusal.
- [ORB-15935] — phase 1, auto-task.
- [ORB-15936] — phase 2, drain and workspace claim.
- [ORB-15937], [ORB-15938], [ORB-15939], [ORB-15940] — phase 3, the task noun in
  four slices.
- [ORB-15941] — phase 4, workflow run.
- [ORB-15942] — phase 5, routine control and search.
- [ORB-15943] — verifies the authorization golden still catches placement and
  permission drift.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

---
title: Operations as Data — Design
owner: claude
last_updated: 2026-10-12
last_validated: 2026-10-12
status: Draft
feature: operations-as-data
doc_role: design
type: design
summary: The operation spec kernel, the split spec/handler table and the derived adapters as built on the friction noun, plus the proposed kernel v2 (effect, aliases, input validation, noun spec, richer CLI parameters) and the planned migration of the remaining nouns.
tags: [operations-as-data, architecture]
paths: ["crates/orbit-common/src/governance/operation.rs", "crates/orbit-common/src/governance/friction/**", "crates/orbit-tools/src/builtin/orbit/operation.rs", "crates/orbit-cli/src/command/operation/args.rs", "crates/orbit-core/src/adapter/tool_host/artifact_redaction.rs", "crates/orbit-common/src/protocol/tool_input.rs"]
related_features: [operations-as-data, orbit-core]
related_artifacts: [ORB-10358]
---

# Operations as Data — Design

This doc describes the friction implementation and the revisions proposed before
any other noun migrates. Each section says whether it is **Implemented** (true
of the code today, on friction only) or **Proposed** (kernel v2, not yet built).
Proposed sections follow the rules in [4_decisions.md](./4_decisions.md), and
those entries are also proposed until the folder returns to `Accepted`. The
order in which nouns migrate is in §9; forward-looking questions are in
[3_vision.md](./3_vision.md).

## 1. Where a fact lives

**Proposed** as a rule; the handler table already follows it.

The friction pilot put every fact it could into the spec, including the CLI's
rendering choice. Facts it could not express as data stayed behind on several
surfaces, sometimes in more than one place. Kernel v2 sorts every fact about a
verb into exactly one of two homes:

| Kind of fact | Home | Examples |
|---|---|---|
| Needed by two or more surfaces, or by the single tool entry every surface traverses | The spec, in `orbit-common` | names, parameters, aliases, effect, redaction class, MCP placement, cross-field presence rules |
| Needed by one surface only | An exhaustive `match` on the verb enum, in that surface's crate | handlers (`orbit-core`), CLI renderers and CLI-only request shaping (`orbit-cli`), MCP schema extensions (`orbit-tools`), claimed-worker routing (`orbit-core`) |

The second home keeps the property that makes the split table work: adding a
verb fails to compile in every surface table that has not decided what to do
with it. A wildcard arm (`_ =>`) in a surface table defeats that and is not
allowed for a migrated noun. See
[The spec holds cross-surface facts; single-surface facts are an exhaustive match in that surface](./4_decisions.md#the-spec-holds-cross-surface-facts-single-surface-facts-are-an-exhaustive-match-in-that-surface).

## 2. The kernel

`orbit_common::governance::operation` holds the vocabulary and nothing else: no
clap types, no axum types, no `OrbitRuntime`. That is what lets it sit in the
leaf crate every surface can already read.

### 2.1 As built (Implemented)

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
    pub cli_render: CliRender,
}
```

A `ParamSpec` carries the wire field name, its `ParamType` (`String`,
`StringList`, `Integer`, `Boolean`), whether it is required, whether an empty
string is meaningful (`preserve_empty`), an **optional** MCP description, and an
**optional** CLI binding. Both sides are optional independently, so a parameter
can be MCP-only, CLI-only, or both. The CLI binding carries its own help text
because MCP and CLI wording legitimately differ: `show`'s `id` is `friction ID`
over MCP and `Friction record ID, e.g. FYYYY-MM-NNN` on the command line.

`Description` is either `Static(&'static str)` or `Computed(fn() -> String)`.
The computed form exists because some descriptions interpolate live
configuration (the friction taxonomy list). Both forms are const-constructible,
so a whole registry stays a `const` item.

### 2.2 Kernel v2 (Proposed)

```rust
pub struct NounSpec<V: 'static> {
    pub cli_name: &'static str,             // "auto-task"
    pub tool_prefix: &'static str,          // "orbit.auto_task"
    pub cli_about: &'static str,            // `orbit auto-task --help` summary
    pub audit_target_type: &'static str,    // "auto_task"
    pub cli_groups: &'static [CliGroup],    // intermediate subcommands: `artifact`, `locks`
    pub operations: &'static [OperationSpec<V>],
}

pub struct OperationSpec<V: 'static> {
    pub verb: V,
    pub cli_path: &'static [&'static str],  // ["add"], or ["artifact", "get"]
    pub tool_name: &'static str,
    pub tool_description: &'static str,
    pub cli_about: &'static str,
    pub effect: Effect,                     // Read | Write
    pub params: &'static [&'static [ParamSpec]], // groups, concatenated in order
    pub at_least_one_of: &'static [&'static str],
    pub retired_fields: &'static [RetiredField], // name + where it went
    pub mcp_scope: Option<McpToolScope>,
}

pub struct ParamSpec {
    pub name: &'static str,
    pub param_type: ParamType,              // + Enum(&'static [&'static str]), Switch, Object
    pub required: bool,
    pub preserve_empty: bool,
    pub aliases: &'static [&'static str],   // accepted wire spellings, resolved at the tool entry
    pub default: Option<&'static str>,
    pub redact: Redaction,                  // None | FreeText | Path
    pub mcp_description: Option<Description>,
    pub cli: Option<CliBinding>,            // CliArgKind::Flag gains `aliases` and `short`
}
```

What each addition is for:

- **`NounSpec`** removes the noun-level strings that friction restates by hand:
  the `"friction"` audit target type and noun name in
  [registry.rs](../../../crates/orbit-cli/src/command/operation/registry.rs),
  the `#[command(about)]` on `FrictionCommand`, and the noun string passed to the
  CLI parser. It also lets one generic CLI impl and one generic tool type serve
  every noun (§6, §7). `cli_name` and `tool_prefix` are separate because
  `auto-task` and `orbit.auto_task` already differ in shipped surfaces.
- **`cli_path`** replaces `name` so that nested commands (`task artifact get`,
  `task locks reserve`) are declared and not hand-wired. The audit subcommand
  label is derived from the path; each migration checks the derived labels
  against the hand-wired arm it replaces, because audit rows are queried by them.
- **`effect`** is §5.
- **Parameter groups** (`&[&[ParamSpec]]`) carry flags shared between verbs,
  such as auto-task's schedule flags (`schedule_args.rs`, used by `add` and
  `update`), without functional record update, which `const` does not allow.
- **`at_least_one_of`** declares the one cross-field rule that recurs:
  friction `update` refuses an edit that changes nothing, and today the CLI
  restates that rule by hand (`require_update_field` in
  [friction.rs](../../../crates/orbit-cli/src/command/friction.rs)).
  Richer cross-field rules stay in handlers.
- **`retired_fields`** generalizes both `rejects_agent_field` and
  `reject_retired_task_add_input_fields`: a field that used to be accepted is
  refused with a message that names where it went.
- **`aliases`** declares the alternative spellings handlers already accept
  silently: `description` for `body`, `task_id` for `during_task`, `tag` for
  `tags`. §4 uses them.
- **`default`**, **`Enum`**, **`Switch`**, and flag `aliases`/`short` are the
  clap features the task and auto-task commands use and the friction kernel
  cannot express: `value_enum` with `default_value_t`, presence flags such as
  `--allow-missing-context`, and `alias = "dependency"`. `Object` is a JSON
  value, for structured inputs such as task relations that are MCP-only today.
- **`redact`** moves the per-verb field lists in
  [artifact_redaction.rs](../../../crates/orbit-core/src/adapter/tool_host/artifact_redaction.rs)
  onto the parameters they name. Today that file lists `title`, `body` and the
  `description` alias for `friction add` by name, so a new free-text parameter
  is unredacted until someone remembers the second file. Nested policies, such
  as the auto-task template's `nested_objects`, stay in the core surface table
  until a second noun needs them.

`cli_render` leaves the spec: rendering is a single-surface fact (§7).

Friction's registry is the first thing ported to v2, with no intended change to
its wire surface, so the existing help goldens and MCP snapshot check the new
kernel before any new noun relies on it.

## 3. The split table

**Implemented.**

One literal table is not practical: handlers need `&OrbitRuntime`, which lives
far above `orbit-common`. Co-locating them would either drag the runtime into the
leaf crate or push the specs above the surfaces that must read them. The shape
that works is **one table split across two crates, joined by the verb enum**:

| Half | Lives in | Form |
|------|----------|------|
| Spec | `orbit-common` | `&'static [OperationSpec<V>]` |
| Handler | `orbit-core` | one exhaustive `match` on `V` |

`FrictionVerb::spec()` is also an exhaustive `match`. Adding a verb therefore
breaks compilation in the spec lookup and in the handler table until it is
fully wired; under §1 it also breaks every other surface table for that noun.
See [Split spec/handler table joined by a typed verb enum](./4_decisions.md#split-spechandler-table-joined-by-a-typed-verb-enum).

The verb enum, its `spec()` match and the ordered operations list must agree,
and a registry test checks that today. Kernel v2 replaces the three
hand-maintained pieces with one small declarative macro per noun that emits all
three from a single list, so the invariant holds by construction.

## 4. Input validation at the tool entry

**Proposed.**

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

**Proposed.**

Whether a friction verb reads or writes is declared four times today:

| Site | What it says |
|---|---|
| `FrictionOperationTool::execution_kind` ([friction/mod.rs](../../../crates/orbit-tools/src/builtin/orbit/friction/mod.rs)) | `list`, `show`, `stats`, `tags` are read-only; everything else (wildcard) is mutating |
| `Commands::Friction` arm ([registry.rs](../../../crates/orbit-cli/src/command/operation/registry.rs)) | only `list` opens the read-only runtime; every other verb opens the full runtime |
| Redaction policy ([artifact_redaction.rs](../../../crates/orbit-core/src/adapter/tool_host/artifact_redaction.rs)) | `add` and `update` redact free text; the rest do not |
| Claimed-worker routing ([worker_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/worker_tools.rs)) | the four reads pass; `add` is rewritten as a claim mutation; other writes are refused |

The first two already disagree: `show`, `stats` and `tags` are read-only tools,
but the CLI opens the full runtime for them, including stale-run reconciliation
on open. Nothing records whether that is deliberate.

Kernel v2 declares `effect: Effect::{Read, Write}` on each spec and derives:

- `ToolExecutionKind` in the generic operation tool (`Read` → `ReadOnly`);
- `RuntimeNeed` in the generic CLI audit arm (`Read` → `RuntimeNeed::ReadOnly`);
- the default claimed-worker route for reads.

`effect` gives the default, not the whole answer. The hand-wired task arm shows
what else the CLI's runtime choice depends on: `task show` and `task artifact
get` route by task ID to the owning workspace (`RuntimeNeed::TaskOwner`), and
`task lint` needs the full runtime only with `--restore-pruned`. ID-addressed
routing is also how `orbit.task.show` resolves over MCP, so it is a
cross-surface fact; phase 3 (§9) decides whether it becomes a spec field or
stays a CLI surface-table override. Input-dependent choices such as
`--restore-pruned` stay in the CLI surface table.

Redaction derives from the parameters' `redact` class (§2.2), not from the
effect. Write-side worker routing stays a core surface table because it carries
real logic (the `add` → claim rewrite). Input-dependent guards also stay in the
handler: friction `update` needs coordination-write permission only when it
moves a record, so it cannot be a per-verb flag.

Porting friction to v2 gives `show`, `stats` and `tags` the read-only CLI
runtime. That is an intended behaviour change, and the PR that makes it must
show that none of the three depends on reconciliation on open.

## 6. MCP adapter

**Implemented:** `orbit_tools::builtin::orbit::operation` holds two generic
functions. `operation_tool_schema(spec)` projects the spec's MCP parameters into
a `ToolSchema` in declaration order. `register_operation(registry, spec, tool)`
registers the tool when `mcp_scope` is `Some`, or keeps it off MCP when the value
is `None`. The scope says only whether dispatch is global or requires a
workspace; it carries no placement or capability policy beyond that.

`FrictionOperationTool(&'static FrictionOperation)` is the single `Tool` impl
that replaced seven. `OrbitBuiltinAction` collapsed its seven `Friction*`
variants into one `Friction(FrictionVerb)`. Downstream matches pattern-match on
the inner verb.

The `list` spec also declares the tool-only `response_mode` parameter (no CLI
flag; `list` is not advertised over MCP, where agents list frictions with
`orbit.search`). Omitting it preserves the legacy record-array response. The
explicit `with_notes` mode has one stable object shape, `{records, notes}`.

**Proposed:**

- **One generic tool.** `OperationTool<V>` replaces the per-noun tool type. A
  kernel trait implemented by each verb enum (emitted by the §3 macro) supplies
  `spec()` and the noun; `orbit-tools` implements `From<V> for
  OrbitBuiltinAction` per noun. `execute` calls `parse_input` (§4) and
  dispatches; `execution_kind` comes from `effect` (§5). Registration loops over
  `NounSpec::operations`.
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
input. `command/friction.rs` supplies the registry and keeps the
`Subcommand`/`FromArgMatches` impls and the response renderers.

Byte-for-byte help compatibility is the load-bearing requirement. It holds
because the adapter reproduces what `#[derive(Args)]` generates:

- the arg id is the wire field name (`during_task`); the flag spelling comes from
  the binding (`--during-task`), and they may differ (`tags` ↔ `--tag`);
- the value name is the field name in SCREAMING_SNAKE (`<DURING_TASK>`);
- args are added in declaration order, and clap assigns display order from
  insertion order for non-positional args, which is why parameter order in the
  spec is contract;
- `Vec`-shaped params use `ArgAction::Append` plus the spec's value delimiter.

Input projection trims **optional** string parameters and drops blank values
unless the parameter declares `preserve_empty`. **Required** parameters pass
through verbatim so the handler reports "you passed only whitespace". The
friction CLI executes through `runtime.run_tool`, so the CLI and MCP reach the
same handler through the same capability chokepoint.

**Proposed:**

- **One generic `Subcommand` impl.** `Invocation<V>` is local to `orbit-cli`, so
  one `impl<V: Verb> Subcommand for Invocation<V>` (and `FromArgMatches`) reads
  the noun from the verb trait. A migrated noun writes no clap impls.
- **Kernel v2 parameter kinds.** `Enum` → `PossibleValuesParser`, so `--help`
  prints `[possible values: …]` as `value_enum` does; `Switch` →
  `ArgAction::SetTrue`, sending `true` only when present; `default` →
  `default_value`, rendering `[default: …]`; flag `aliases` → `.alias()`
  (hidden, like derive's `alias`); `cli_path` → intermediate subcommands built
  from `NounSpec::cli_groups`.
- **No interning leak.** The adapter `Box::leak`s each SCREAMING_SNAKE value
  name because `value_name` was assumed to need `&'static str`. The workspace
  already enables clap's `string` feature, so `value_name` takes an owned
  `String` and the leak goes.
- **Renderers as a surface table.** `CliRender` leaves the spec. Each noun's CLI
  module provides an exhaustive `fn render(verb, &Value) -> CommandOut` and, when
  it needs one, an exhaustive `fn prepare(verb, &mut Map<String, Value>)` for
  CLI-only request shaping (friction `list` injects `response_mode`). The kernel
  no longer carries noun-specific variants such as `CliRender::TagList`.
- **Cross-field errors named in CLI terms.** When `parse_input` refuses an
  `at_least_one_of` violation, the CLI maps each field to its flag spelling from
  the spec's bindings (`tags` → `--tag`). `require_update_field` and its
  hand-maintained `UPDATE_FIELD_KEYS` list are deleted.
- **One generic audit arm.** `Commands::<Noun>` arms in
  [registry.rs](../../../crates/orbit-cli/src/command/operation/registry.rs)
  call one helper that derives `RuntimeNeed` from `effect` and `admin_meta` from
  `audit_target_type`, `cli_path` and the positional target. Arms that need
  more, such as `governed_when` for a flag-gated destructive command, add it
  around the helper.
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

**Proposed:** once §4 lands, the tool entry refuses an undeclared field for every
caller, and `FrictionCall::set`'s own check becomes redundant. It stays as a
thin builder; it is not copied to other nouns.

## 9. Migration plan

**Proposed.** This replaces the touch-it-move-it ratchet; see
[Migrate the remaining nouns on a plan, not on contact](./4_decisions.md#migrate-the-remaining-nouns-on-a-plan-not-on-contact).

Each phase is one Orbit task tagged `operations-as-data` (large nouns split per
slice). Each starts with cookbook Step 0 and puts its behaviour diff in the PR.

| Phase | Scope | Why here |
|---|---|---|
| 0 | Kernel v2, ported onto friction | Proves v2 against existing goldens with no new noun at stake; the only intended behaviour change is §5's read-only runtime for three verbs, plus unknown-field warnings |
| 1 | auto-task: 5 tools (`show` inactive), 10 CLI verbs | Second pilot. First noun whose CLI calls core directly; exercises `Enum`, `default`, parameter groups, CLI-only verbs (`toggle`, `delete`, `restore`, `recover`, `reset` with `mcp_scope: None`) and the tool-path rule |
| 2 | drain (6 tools) and workspace claim (3 inactive tools) | Tool-only, mechanical; collapses nine `OrbitBuiltinAction` variants; drain verbs keep their agent-floor rows in `GOVERNED_OPERATIONS` |
| 3 | task, in slices: reads (`show`, `list`, `eligible`, `lint`); writes (`add`, `update`, with the guarded schema extension); `artifact get`/`put` (nested); `locks` (nested); review (`reconcile_review`, `review_reset`, `reject`); `delete` | Largest noun; slices keep each PR reviewable. `flow`, `export`, `import`, `publication`, `reindex`, `recheck-blocked` and `archive` have no tool and are judged per subcommand, not migrated by default |
| 4 | workflow run: `workflow.ship`, `workflow.run.show/list/resume`, `workflow.auto` | Operator-governed; the authorization matrix golden in `output_goldens.rs` must not move except as intended |
| 5 | Single-verb tools with a second surface: `routine.control` with `orbit routine pause/resume`; `search` with `orbit search` | A one-verb noun with one surface gains nothing; `agent.invoke`, `command.exec` and `pipeline.invoke/wait` migrate only if they gain a surface |

Out of scope: the `Desktop*` host actions (a separate host protocol), GitHub and
process tools, and plugin tools, which have their own declaration format.

Progress is measured, not remembered: the count of hand-written `impl Tool for`
under `crates/orbit-tools/src/builtin/orbit/`, and the number of `OrbitBuiltinAction`
variants other than the `Desktop*` host actions and the nouns' single
`Noun(Verb)` variants. On 2026-10-12 those were 41 and 36 (friction is the one collapsed
noun). The weekly code-health sweep reports both numbers. Neither is a merge
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

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

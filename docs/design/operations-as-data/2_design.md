---
title: Operations as Data — Design
owner: claude
last_updated: 2026-10-13
last_validated: 2026-10-13
status: Accepted
feature: operations-as-data
doc_role: design
type: design
summary: The operation spec kernel v2 (noun spec, effect, redaction class, parameter groups, aliases, retired fields, at-least-one-of, switch/enum/enum-list/object and CLI-only parameters, CLI help beyond the summary, input validation at the tool entry in warn mode, generic tool and CLI adapters) as built on friction, auto-task, drain, workspace claim and the task read and write verbs, plus the planned migration of the remaining nouns.
tags: [operations-as-data, architecture]
paths: ["crates/orbit-common/src/governance/operation.rs", "crates/orbit-common/src/governance/drain.rs", "crates/orbit-common/src/governance/workspace_claim.rs", "crates/orbit-common/src/governance/friction/**", "crates/orbit-tools/src/builtin/orbit/operation.rs", "crates/orbit-cli/src/command/operation/args.rs", "crates/orbit-cli/src/command/operation/spec.rs", "crates/orbit-cli/src/command/friction.rs", "crates/orbit-common/src/governance/auto_task/**", "crates/orbit-tools/src/builtin/orbit/auto_task.rs", "crates/orbit-cli/src/command/auto_task/**", "crates/orbit-core/src/adapter/tool_host/auto_task_tools.rs", "crates/orbit-core/src/adapter/tool_host/artifact_redaction.rs", "crates/orbit-core/src/adapter/tool_host/worker_tools.rs", "crates/orbit-common/src/protocol/tool_input.rs", "crates/orbit-core/src/adapter/tool_host/friction_tools.rs", "crates/orbit-web/src/api/frictions.rs", "crates/orbit-common/src/governance/task/**", "crates/orbit-tools/src/builtin/orbit/task/operation.rs", "crates/orbit-core/src/adapter/tool_host/task_operation_tools.rs", "crates/orbit-core/src/adapter/tool_host/task_write_tools.rs", "crates/orbit-tools/src/builtin/orbit/task/guarded.rs", "crates/orbit-cli/src/command/task/operation.rs"]
related_features: [operations-as-data, orbit-core]
related_artifacts: [ORB-10358, ORB-15932, ORB-15933, ORB-15935, ORB-15936, ORB-15937, ORB-15938]
---

# Operations as Data — Design

This doc describes the friction implementation and the revisions accepted on
2026-10-12 for every noun that migrates after it. Each section says whether it
is **Implemented** (true of the code today, on the migrated nouns) or
**Planned** (accepted but not yet built). Kernel v2's structure, its richer
parameter kinds and its input validation are implemented, the validation in
warn mode for unknown fields except on the task writes, which refuse them. Both follow the rules in
[4_decisions.md](./4_decisions.md). As a phase lands, its sections move from
Planned to Implemented. The order in which nouns migrate is in §9;
forward-looking questions are in
[3_vision.md](./3_vision.md).

## 1. Where a fact lives

**Implemented** for friction, auto-task, drain, workspace claim and the task
read verbs; the rule binds every noun that migrates.

The friction pilot put every fact it could into the spec, including the CLI's
rendering choice. Facts it could not express as data stayed behind on several
surfaces, sometimes in more than one place. Kernel v2 sorts every fact about a
verb into exactly one of two homes:

| Kind of fact | Home | Examples |
|---|---|---|
| Needed by two or more surfaces, or by the single tool entry every surface traverses | The spec, in `orbit-common` | names, parameters, effect, redaction class, MCP placement, aliases, retired fields and cross-field presence rules |
| Needed by one surface only | An exhaustive `match` on the verb enum, in that surface's crate | handlers (`orbit-core`), CLI renderers and CLI-only request shaping (`orbit-cli`), claimed-worker routing (`orbit-core`); later MCP schema extensions (`orbit-tools`) |

The second home keeps the property that makes the split table work: adding a
verb fails to compile in every surface table that has not decided what to do
with it. A wildcard arm (`_ =>`) in a surface table defeats that and is not
allowed for a migrated noun. Friction's tables in `friction_tools.rs`,
`worker_tools.rs`, `artifact_redaction.rs` and `command/friction.rs` follow it,
as do auto-task's in `auto_task_tools.rs`, the same two core tables,
`builtin/orbit/auto_task.rs` (MCP guarded modes) and `command/auto_task/`.
The task read verbs' tables are `task_operation_tools.rs`, the same two core
tables, `builtin/orbit/task/operation.rs` (MCP guarded modes),
`command/task/operation.rs` and `task_operation` in `command/operation/spec.rs`.
See
[The spec holds cross-surface facts; single-surface facts are an exhaustive match in that surface](./4_decisions.md#the-spec-holds-cross-surface-facts-single-surface-facts-are-an-exhaustive-match-in-that-surface).

## 2. The kernel

`orbit_common::governance::operation` holds the vocabulary and nothing else: no
clap types, no axum types, no `OrbitRuntime`. That is what lets it sit in the
leaf crate every surface can already read.

### 2.1 As built (Implemented)

```rust
pub struct NounSpec<V: 'static> {
    pub cli_name: &'static str,             // "friction", "auto-task"
    pub tool_prefix: &'static str,          // "orbit.friction", "orbit.auto_task"
    pub tool_only_registration: ToolOnlyRegistration, // Active | Inactive off MCP
    pub cli_about: &'static str,            // `orbit friction --help` summary
    pub audit_target_type: &'static str,    // "friction"
    pub audit_target_param: &'static str,   // "id"; auto-task's is "name"
    pub cli_groups: &'static [CliGroup],    // intermediate subcommands; none so far
    pub operations: &'static [OperationSpec<V>],
}

pub struct OperationSpec<V: 'static> {
    pub verb: V,                       // typed verb, joins spec ↔ every surface table
    pub cli_path: &'static [&'static str], // ["add"]; ["artifact", "get"] later
    pub tool_name: &'static str,       // "orbit.friction.add"
    pub tool_description: &'static str,
    pub cli_about: &'static str,
    pub cli_help: CliHelp,             // long about, after-help, flag order (§2.2)
    pub effect: Effect,                // Read | Write
    pub params: &'static [&'static [ParamSpec]], // groups, concatenated in order
    pub at_least_one_of: &'static [&'static str],
    pub retired_fields: &'static [RetiredField], // name + where it went
    pub unknown_fields: UnknownFields,  // Warn | Refuse (§4)
    pub mcp_scope: Option<McpToolScope>,
}

pub struct ParamSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str], // accepted wire spellings, resolved at the tool entry
    pub param_type: ParamType,         // String | StringList | Integer | Boolean
                                       // | Switch | Enum(&[EnumValue]) | Object | Array (§2.2)
    pub required: bool,
    pub preserve_empty: bool,
    pub default: Option<&'static str>, // CLI default, shown as `[default: …]` (§2.2)
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
- **Parameter groups** (`&[&[ParamSpec]]`) carry flags shared between verbs
  without functional record update, which `const` does not allow.
  `OperationSpec::params()` iterates every group in order. Friction and
  auto-task declare one group per verb: auto-task's `add` and `update` schedule
  and template flags differ in help text and requiredness, so they share
  nothing a group could carry.
- **`audit_target_param`** names the parameter whose value is the CLI audit
  row's `target_id`. It replaced the positional parameter, because auto-task
  `add` takes its name as `--name`.
- **`redact`** moves the per-verb field lists in
  [artifact_redaction.rs](../../../crates/orbit-core/src/adapter/tool_host/artifact_redaction.rs)
  onto the parameters they name, so a new free-text parameter is redacted where
  it is declared. Friction's `add` and `update` mark `body` and `title`
  `FreeText`, and an alias takes its parameter's class. Nested policies such as
  the auto-task `template` and `template_patch` objects' free text stay in the
  core surface table until a second noun needs them.
- **`aliases`** declares the alternative spellings a verb accepts: friction's
  `description` for `body`, `task_id` for `during_task`, and `tag` for `tags`.
  The tool entry (§4) renames them, so handlers read only canonical names.
- **`at_least_one_of`** declares the one cross-field rule that recurs: friction
  `update` refuses an edit that changes nothing. The tool entry names the input
  fields; the CLI adapter checks the same list first and names the flags
  instead. Richer cross-field rules stay in handlers.
- **`retired_fields`** replaces `rejects_agent_field`: a field that used to be
  accepted is refused with a message that names where it went. Friction `add`
  retires `agent` (`RETIRED_AGENT_FIELD`). Its `wording` keeps a shipped
  refusal's phrasing: `no_longer_accepts` (friction's), `unknown_field` (task
  `add`'s `plan`, `status`, `parent_id` and the other creation-time fields,
  which name `orbit.task.update`), or `verbatim` (task `update`'s
  `required_tools` spellings, `force` and `artifacts`).
- **`CliGroup`** names an intermediate subcommand and its `--help` summary. No
  migrated noun declares one yet; the CLI adapter builds groups when the first
  nested noun (task, phase 3) needs them.
- **`OperationVerb`** is what the generic adapters are generic over. The
  `operation_registry!` macro (§3) implements it.

`cli_render` left the spec: rendering is a single-surface fact (§7).

### 2.2 Kernel v2 parameter kinds (Implemented)

```rust
pub enum ParamType {
    String, StringList, Integer, UnsignedInteger, Boolean,
    Switch,                          // presence flag on the CLI; boolean on the wire
    Enum(&'static [EnumValue]),      // fixed values; a string on the wire
    EnumList(&'static [EnumValue]),  // several fixed values; a string list on the wire
    Object,                          // a JSON object, for structured input
    Array,                           // a JSON array of structured entries
}

pub struct EnumValue {
    pub name: &'static str,          // CLI spelling
    pub help: Option<&'static str>,  // per-value `--help` line
    pub aliases: &'static [&'static str], // hidden spellings
}

pub enum CliArgKind {
    Positional,
    Flag {
        long: &'static str,
        delimiter: Option<char>,
        aliases: &'static [&'static str],         // hidden, like derive's `alias`
        visible_aliases: &'static [&'static str], // listed, like `visible_alias`
        value_name: Option<&'static str>,         // when not the field name
        multiple: bool,                           // `--fields id title`: one flag, many values
    },
}

pub struct CliHelp {
    pub long_about: Option<&'static str>,  // `--help` description; `-h` keeps `cli_about`
    pub after_help: Option<&'static str>,  // examples printed after the options
    pub flag_order: &'static [&'static str], // `--help` order, when not declaration order
}
```

What each addition is for:

- **`default`**, **`Enum`**, **`Switch`**, and flag aliases are the clap
  features the auto-task (and later task) commands use and the friction kernel
  could not express: `value_enum` with `default_value_t`, presence flags such as
  `--force`, `alias = "required-tool"` and `visible_alias = "all"`. `Object` is
  a JSON value, for structured inputs such as auto-task's `schedule` and
  `template`. `short` was not needed by auto-task and is not built.
- **CLI-only parameters.** A parameter with a CLI binding and no MCP
  description is CLI-only: it is not a wire field. `wire_params()` leaves it
  out, so `parse_input` treats it as undeclared, and the noun's CLI `prepare`
  table consumes it before the call. Auto-task's `--cron`, `--every-minutes`
  and `--deliveries-landed` compose `schedule`; `--title`, `--body`,
  `--criterion` and the other template flags compose `template` (`add`) or
  `template_patch` (`update`); `--waive-batch` and `--waiver-reason` compose
  `waive_batch`; `list`'s `--enabled` and `--disabled` filter the response.
  A parameter with neither binding is a hidden wire field: `update`'s
  `template_patch` is accepted by the tool and advertised nowhere.
- **`EnumList`, `multiple`, `CliHelp` and an optional positional** are what
  the task read verbs (phase 3a) needed. `list --status` and `eligible
  --status` take several enum values (`EnumList`, a `string_list` on the
  wire); `show --fields id title` takes several values after one flag
  (`multiple`); `lint [ID]` takes an optional positional. `CliHelp` carries
  the long description and examples `show` and `list` print, and
  `flag_order` keeps `--help` in the hand-written order where the MCP schema
  lists parameters differently. Every other spec declares `CliHelp::NONE`.
  The parameter builders (`both`, `mcp_only`, `cli_only`, `required` and the
  other modifiers) moved into the kernel from the auto-task registry, and the
  task enum value tables moved to the task registry, so both nouns share them.
- **`Array`, `CliBinding::required` and hidden option objects** are what the
  task writes (phase 3b) needed. `relations` is an array of typed entries
  (`Array`). `add --complexity` is required on the CLI while guarded creation
  over MCP may omit it (`cli_required`). The operator command's options that
  MCP must not see (`add --plan`, `--status`, `--parent`; `update --approve`,
  `--force`, `--artifact`) travel in one `cli_options` object that is a
  hidden wire field, consumed by the core handler (§9, phase 3b).
- **Enum membership is a content rule.** The tool entry checks only that an
  `Enum` value is a string; the handler (or the serde type it deserializes
  into) owns the allowed set. The CLI spells values in kebab-case
  (`skip-if-open`, `in-progress`) and the wire in snake_case; the auto-task
  `prepare` table converts.

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

**Implemented (warn mode)** on friction, auto-task, drain, workspace claim and
the task read verbs: unknown fields are reported, not yet refused. The task
writes declare `unknown_fields: Refuse`, because `orbit.task.add` and
`orbit.task.update` refused unknown fields before they migrated; the refusal
names the field and suggests a declared one.

`OperationSpec::parse_input(&self, input: Value) -> Result<OperationInput,
OrbitError>` in `orbit-common` checks a call against its spec, reusing the
helpers in [tool_input.rs](../../../crates/orbit-common/src/protocol/tool_input.rs).
In order, it:

1. requires a JSON object (an absent input counts as `{}`);
2. resolves each declared alias to its canonical name, refusing input that sets
   both and naming both;
3. refuses retired fields with their hint;
4. collects unknown fields — anything that is not a parameter, an alias, a
   retired field, a transport wrapper key (`_meta`, `workspace`) or an identity
   key (`model`, `agent`, which session identity may also supply);
5. checks each present value against its `ParamType` (`StringList` keeps
   accepting a string, an array, or a comma-separated string) and drops `null`
   values, which mean "absent";
6. checks that required parameters are present, with the existing did-you-mean
   hint, and that `at_least_one_of` is met. An identity key that is a required
   parameter is left to identity resolution, because the session may supply it.

It checks presence and shape only. Content rules, such as refusing a
whitespace-only required value or validating a status name, stay in the handler
where the domain rules live; that preserves the trimming behaviour the friction
migration was careful to keep.

The generic operation tool (§6) calls `parse_input` before dispatch. Every
surface reaches a tool through `Tool::execute`: MCP, `orbit tool run`, the CLI's
`run_tool`, the dashboard, and the deterministic dispatcher. So one call site
gives every surface the same validation. Handlers read canonical names from
`OperationInput` and never see an alias; the friction handlers re-parse at
their own entry, which is idempotent, so a direct core call gets the same
canonical input.

Refusing unknown fields is a wire change for any caller that sends extra fields
today. Each noun turns it on in two steps: first a `warn!` and an audit row per
unknown field, then refusal after the 30-day compatibility window. In warn mode
the tool entry logs `<tool> ignored unknown input field `<field>`` and writes
one system audit row per field (`command` `tool_input`, `subcommand`
`unknown_field`, `tool_name` the tool, arguments naming the tool, field and
actor), then runs the call. The field stays in the input during warn mode, so
behaviour that already read it, such as a claimed worker's argument binding
check, is unchanged. See
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

`effect` gives the default, not the whole answer. The task arm shows what else
the CLI's runtime choice depends on: `task show` and `task artifact get` bind
the workspace that owns the task ID (`RuntimeNeed::TaskOwner`), and `task lint`
needs the full runtime only with `--restore-pruned`. For the migrated task
verbs both overrides live in `task_operation`
([spec.rs](../../../crates/orbit-cli/src/command/operation/spec.rs)), the task
noun's CLI table around `registry_operation`. `lint` declares `Write`, because
`--restore-pruned` writes, and the table opens the read-only runtime without
the flag.

**ID routing stays keyed by tool name, outside the spec** (phase 3a's answer).
Two tool-name lists hold it, and every surface derives from them:
`ID_ROUTED_TASK_TOOLS` in
[mcp_rules.rs](../../../crates/orbit-tools/src/mcp_rules.rs) (a call without a
workspace selector goes to the host its task-ID prefix names, for the
federated MCP and the CLI host preflight in `host_route.rs`), and
`ID_RESOLVED_WORKSPACE_TOOLS` in the CLI's MCP server (the call binds the
workspace that owns the ID, for MCP, `orbit tool run` and now `task_operation`).
`NOT_ID_ROUTED_TOOLS` records why a required-ID tool such as
`orbit.friction.show` keeps selector behaviour. `lint` accepts an optional ID
for its workspace sweep and stays outside the required-ID tables; both forms
keep the selected workspace. See
[ID routing is keyed by tool name in the MCP rule tables, not declared in the spec](./4_decisions.md#id-routing-is-keyed-by-tool-name-in-the-mcp-rule-tables-not-declared-in-the-spec).

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
`effect` (§5); `execute` checks the input with `parse_input` (§4), reports any
unknown fields, and dispatches the verb's action with the canonical input. `register_noun(registry, &NOUN)` registers
each operation of a noun at its `mcp_scope`, or keeps it off MCP when the value
is `None`, using the noun's `tool_only_registration` to preserve active versus
inactive local availability. The scope says only whether dispatch is global or
requires a workspace; it carries no placement or capability policy beyond that.

`OrbitBuiltinAction` collapsed friction's seven `Friction*` variants into one
`Friction(FrictionVerb)`, and auto-task's five into `AutoTask(AutoTaskVerb)`;
phase 2 similarly replaces six drain variants with `Drain(DrainVerb)` and three
workspace-claim variants with `WorkspaceClaim(WorkspaceClaimVerb)`, and phase
3a replaces the four task read variants with `Task(TaskVerb)`, and phase 3b
folds `TaskAdd` and `TaskUpdate` into it; the other task variants stay until
their slices land.
`orbit-tools` implements `From<V>` for each. Downstream matches pattern-match
on the inner verb. Friction needs no module of its own in `orbit-tools`.

Auto-task has one MCP surface table, `GuardedModes` in
[auto_task.rs](../../../crates/orbit-tools/src/builtin/orbit/auto_task.rs), an
exhaustive match on `AutoTaskVerb` that `OperationTool::execute` consults after
`parse_input`. Three verbs have an input-selected mode that routes to a desktop
host action instead of the handler: `list` with `view: "bounded"` (bounded
read), `update` with `expected_enabled` (operator-checked toggle) and `mint`
with `acknowledge_unconditional` (operator-acknowledged mint). Every other verb,
every friction verb, and every drain/workspace-claim verb return no mode.
The task table in
[task/operation.rs](../../../crates/orbit-tools/src/builtin/orbit/task/operation.rs)
does the same for `show` (`view: "bounded"`, and `snapshot: true` for the
versioned snapshot) and `list` (`view: "bounded"`); `eligible` and `lint`
return none. `GuardedModes::preempt` runs before `parse_input`, for a mode
whose input the spec's flat parameter list does not describe: guarded
creation and guarded update (`add` and `update` carrying `request_id`,
`expected_revision`, `verdict` or `complete`) keep their own field rules, and `update` refuses a managed
implementation activity setting `blocked`, and an explicit `crew: null`,
with their shipped messages.

**Schema extensions** are the same table's `schema_extension(verb)`, an
exhaustive match applied after the derived schema. Only `orbit.task.add` and
`orbit.task.update` return one
([guarded.rs](../../../crates/orbit-tools/src/builtin/orbit/task/guarded.rs)):
the JSON Schema conditionals for guarded creation and update, which the flat
parameter list cannot express and only MCP needs. The MCP snapshot did not
move.

The `list` spec also declares the tool-only `response_mode` parameter (no CLI
flag; `list` is not advertised over MCP, where agents list frictions with
`orbit.search`). Omitting it preserves the legacy record-array response. The
explicit `with_notes` mode has one stable object shape, `{records, notes}`.

**Decided (phase 1):** an `Enum` parameter keeps its allowed values, and any
default, in its MCP description, and the schema emits no `enum` or `default`
keyword. `ParamType::Enum` maps to the `string` tool type and a `default`
affects only the CLI. Auto-task's migration is therefore wire-identical: the MCP
snapshot did not move. Emitting the keywords stays possible later as an
additive change that re-blesses the snapshot.

## 7. CLI adapter

**Implemented:** `orbit_cli::command::operation::args` builds a `clap::Command`
subcommand per registry entry and projects parsed matches back into the tool
input. It implements `Subcommand` and `FromArgMatches` once, for
`Invocation<V>` over every `V: OperationVerb`, reading the operations and noun
name from `V::noun()`; a migrated noun writes no clap impls. The noun's parent
struct holds `#[command(subcommand)] command: Invocation<FrictionVerb>` (or
`Invocation<AutoTaskVerb>`).

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
- `Vec`-shaped params use `ArgAction::Append` plus the spec's value delimiter;
- `Enum` uses a `PossibleValuesParser` whose values carry their help and hidden
  aliases, so `--help` prints `[possible values: …]` or one value per line as
  `value_enum` does, and a parsed alias is sent as its value's name;
- `Switch` is `ArgAction::SetTrue` and sends `true` only when present;
- `default` is `default_value`, rendering `[default: …]`, and the CLI sends it;
- flag `aliases` and `visible_aliases` are `.aliases()` and
  `.visible_aliases()`, and `value_name` overrides the SCREAMING_SNAKE name.

Input projection trims **optional** string parameters and drops blank values
unless the parameter declares `preserve_empty`. **Required** parameters pass
through verbatim so the handler reports "you passed only whitespace". The
friction CLI executes through `runtime.run_tool`, so the CLI and MCP reach the
same handler through the same capability chokepoint.

[friction.rs](../../../crates/orbit-cli/src/command/friction.rs) keeps only
friction's two CLI surface tables, both exhaustive matches on `FrictionVerb`:
`prepare(verb, &mut Map)` shapes the request (`list` injects `response_mode`)
and `render(verb, &Value)` picks the record, table, tag-list or JSON rendering.
The kernel carries no rendering vocabulary.
[command/auto_task/](../../../crates/orbit-cli/src/command/auto_task/) has the
same two tables over `AutoTaskVerb`: `prepare` composes the CLI-only flags into
structured fields and refuses conflicting schedule or waiver flags, and
`render` reproduces the text output of the hand-written commands it replaced.

**Implemented for task reads: a noun can be part migrated.** `orbit task`
keeps its hand-wired verbs beside the migrated ones: `TaskSubcommand` holds
a flattened `Operation(Invocation<TaskVerb>)` variant, which contributes the
registry's subcommands to the same clap tree. Its comment must be `//`, not
`///`, because clap renders a doc comment on a flattened variant as help.
[command/task/operation.rs](../../../crates/orbit-cli/src/command/task/operation.rs)
holds `prepare` (`list` consumes `--all`, `--ops` and `--full`; `show` consumes
`--host`, which the host preflight already resolved) and `render`, which calls
each verb's text renderer in `list.rs`, `eligible.rs`, `show.rs` and `lint.rs`.
The renderers build text from the tool's JSON; the runtime lookups they still
make (relation statuses, the bound workspace identity) are presentation, not
the verb's answer.

Cross-field errors are named in CLI terms. Before the call,
`Invocation::require_at_least_one_of` checks the spec's `at_least_one_of` and,
when nothing is set, names each field by its flag spelling from the spec's
bindings (`tags` → `--tag`): `nothing to update; pass at least one of
`--status`, `--tag`, …`. The same call over MCP or `orbit tool run` is refused
by the tool entry naming the fields.

The `Commands::Friction` arm in
[registry.rs](../../../crates/orbit-cli/src/command/operation/registry.rs)
calls one helper, `registry_operation`, which derives `RuntimeNeed` from
`effect` and `admin_meta` from `cli_name`, `audit_target_type`, `cli_path` and
the `audit_target_param` value. The `Commands::AutoTask` arm is the same one
call. Arms that need more, such as `governed_when` for a flag-gated destructive
command, add it around the helper.

**Implemented for auto-task and the task reads: the CLI always goes through the
tool.** Every migrated verb executes by `run_tool`, including the CLI-only ones,
which are tools with `mcp_scope: None` (`orbit.task.lint` is one). For nouns
whose CLI called core services directly (auto-task, task, and later routine and
workflow run) this is a behaviour change, not a refactor: capability
resolution, audit rows, attribution (`mutation_identity` versus the `model`
parameter) and write screening move to the tool path. The measured differences
for auto-task and the task reads are in
[Migrated CLI verbs execute through the tool path](./4_decisions.md#migrated-cli-verbs-execute-through-the-tool-path).

**Planned:** `cli_path` → intermediate subcommands built from
`NounSpec::cli_groups`, when the first nested noun (task, phase 3) needs them.

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

Request-body fields the verb's spec does not declare are kept by a flattened
`extra` map on the create and patch bodies and forwarded to the call, so the
tool entry reports them (§4) as it does for every other surface. Declared
fields a route does not model stay out of the call, so forwarding adds no
dashboard capability.

**Planned:** once unknown fields are refused (phase 0b's second step), the tool
entry refuses an undeclared field for every caller, and `FrictionCall::set`'s
own check becomes redundant. It stays as a thin builder; it is not copied to
other nouns.

## 9. Migration plan

**Planned**, except phase 0a, phase 0b's warn mode, phases 1–2 and phase 3's
read and write slices, which are implemented. This replaces the touch-it-move-it ratchet; see
[Migrate the remaining nouns on a plan, not on contact](./4_decisions.md#migrate-the-remaining-nouns-on-a-plan-not-on-contact).

Each phase is one Orbit task tagged `operations-as-data` (large nouns split per
slice); the task IDs follow the table. Each starts with cookbook Step 0 and puts its behaviour diff in the PR.

| Phase | Scope | Why here |
|---|---|---|
| 0a (done) | Kernel v2 structure, ported onto friction: noun spec, `cli_path`, `effect`, parameter groups, `redact`, the registry macro, generic tool and CLI adapters, CLI surface tables | Proves v2 against existing goldens with no new noun at stake; the only behaviour change is §5's read-only runtime for three verbs |
| 0b (warn mode done) | Kernel v2 input validation on friction: `parse_input`, aliases, retired fields, `at_least_one_of`, unknown-field warnings | Moves the friction handlers onto canonical names and the warn-then-refuse rollout onto the one migrated noun |
| 1 (done) | auto-task: 5 tools (`show` inactive), 10 CLI verbs | Second pilot. First noun whose CLI calls core directly; exercises `Enum`, `default`, parameter groups, CLI-only verbs (`toggle`, `delete`, `restore`, `recover`, `reset` with `mcp_scope: None`) and the tool-path rule |
| 2 (done) | drain (6 tools) and workspace claim (3 inactive tools) | Tool-only, mechanical; collapses nine `OrbitBuiltinAction` variants; drain verbs keep their agent-floor rows in `GOVERNED_OPERATIONS` |
| 3 (reads, writes done) | task, in slices: reads (`show`, `list`, `eligible`, `lint`); writes (`add`, `update`, with the guarded schema extension); `artifact get`/`put` (nested); `locks` (nested); review (`reconcile_review`, `review_reset`, `reject`); `delete` | Largest noun; slices keep each PR reviewable. `flow`, `export`, `import`, `publication`, `reindex`, `recheck-blocked` and `archive` have no tool and are judged per subcommand, not migrated by default |
| 4 | workflow run: `workflow.ship`, `workflow.run.show/list/resume`, `workflow.auto` | Operator-governed; the authorization matrix golden in `output_goldens.rs` must not move except as intended |
| 5 | Single-verb tools with a second surface: `routine.control` with `orbit routine pause/resume`; `search` with `orbit search` | A one-verb noun with one surface gains nothing; `agent.invoke`, `command.exec` and `pipeline.invoke/wait` migrate only if they gain a surface |

Tasks: phase 0a is [ORB-15932] (kernel v2 structure, done); phase 0b is
[ORB-15933] (input validation, warn mode, done) and [ORB-15934] (refuse unknown
fields after the window); phase 1
[ORB-15935] (done); phase 2 [ORB-15936] (done); phase 3 [ORB-15937] (reads, done), [ORB-15938]
(add/update, done), [ORB-15939] (artifact and locks), [ORB-15940] (review and
lifecycle); phase 4 [ORB-15941]; phase 5 [ORB-15942]. [ORB-15943] verifies the
authorization golden that phases 1 and 4 rely on.

Out of scope: the `Desktop*` host actions (a separate host protocol), GitHub and
process tools, and plugin tools, which have their own declaration format.

Progress is measured, not remembered: the count of hand-written `impl Tool for`
under `crates/orbit-tools/src/builtin/orbit/`, and the number of `OrbitBuiltinAction`
variants other than the `Desktop*` host actions and the nouns' single
`Noun(Verb)` variants. On 2026-10-12 those were 41 and 36 (friction is the one collapsed
noun, and the generic `OperationTool<V>` is one of the 41). On 2026-10-13, after phase 1,
they are 36 and 32: auto-task's five tool impls and five variants are gone (37
variants were counted at phase 1's base). After phase 2 on the same date, they
are 27 and 23: nine per-verb tool impls and nine action variants are replaced by
the generic tool and two noun variants. After phase 3a on the same date, they
are 23 and 19: the four task read tool impls and action variants are replaced
by the generic tool and `Task(TaskVerb)`. After phase 3b on the same date,
they are 21 and 18: the `add` and `update` tool impls and action variants are
gone, and `orbit.task.artifact.put`, which dispatched `TaskUpdate`, has its own
`TaskArtifactPut` variant until its slice lands. The weekly code-health sweep reports
both numbers. Neither is a merge gate.

### Phase 2: tool-only nouns (Implemented)

Drain and workspace claim use `OperationTool<V>` and one action variant per
noun. Their handler, guarded-mode, worker-route and redaction tables match
exhaustively on `DrainVerb` or `WorkspaceClaimVerb`; they derive no CLI or
dashboard surface. All nine tools stay off MCP. `NounSpec::tool_only_registration`
distinguishes drain's six active registrations from workspace claim's three
inactive registrations, and existing friction/auto-task non-MCP registrations
remain inactive. MCP-scoped operations still use `register_mcp`.

Drain uses the common `orbit` tool prefix, with each operation declaring its
full shipped name, so `orbit.task.pull` stays part of this noun without a rename.
The workspace-claim TTL retains its historical `u64` schema token via
`ParamType::UnsignedInteger`, with the same non-negative integer validation as
`Integer`, while the generic CLI adapter projects it as a `u64` when bound.
Its `ttlSeconds` and `ttl-seconds` aliases are declared on
`ttl_seconds`; probe's ignored legacy review-policy field stays hidden from the
schema. Workspace-claim show retains the old tool's conservative write effect.
No governed-operation row changes.

Unknown fields warn and audit at the generic tool entry, then handlers ignore
them. Pull still checks protocol fingerprint skew **before** input-shape
validation: `OrbitToolHost::prepare_operation_input` delegates to the drain's
exhaustive compatibility table before `parse_input`, and the drain handler
repeats this ordering for direct host calls. This preparation also preserves
`AdmissionRequest`'s historical omitted `caller_before_pr` → `false` default
without changing its advertised requiredness. Typed request decoding and replay
comparison remain in the owner application. Supplied structural claim values
are not redacted or rewritten.

### Phase 3a: task reads (Implemented)

`orbit.task.list`, `orbit.task.eligible`, `orbit.task.show` and the CLI-only
`orbit.task.lint` are declared in
[governance/task/operations.rs](../../../crates/orbit-common/src/governance/task/operations.rs)
and served by `OperationTool<TaskVerb>`. The handlers in
[task_operation_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/task_operation_tools.rs)
answer every surface, so the CLI and MCP read the same record: the full `show`
record carries `next_step` for a run-blocked task and the `final_recovery`
decision, and the `lint` sweep and the blocked-task next step moved from the
CLI into `orbit-core` (`sweep_context_repairs`, `blocked_next_step`). The
help goldens under `crates/orbit-cli/tests/help_goldens/task/` were captured
before the migration and pass unchanged. The MCP snapshot moved only for
`orbit.task.list`, which now advertises the `external_ref` and `has_ref`
filters the CLI already had and describes `priority` as a filter in every view
rather than only the bounded one. The behaviour baseline's differences are
listed in
[Migrated CLI verbs execute through the tool path](./4_decisions.md#migrated-cli-verbs-execute-through-the-tool-path).

### Phase 3b: task writes (Implemented)

`orbit.task.add` and `orbit.task.update` are declared in
[write_operations.rs](../../../crates/orbit-common/src/governance/task/write_operations.rs)
and served by `OperationTool<TaskVerb>`. `orbit task add` and `orbit task
update` execute through `run_tool`, and the one handler in
[task_write_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/task_write_tools.rs)
assembles `TaskAddParams` and `TaskUpdateParams` for every surface. The help
goldens under `crates/orbit-cli/tests/help_goldens/task/` (`add.txt`,
`update.txt`) were captured before the migration and pass unchanged, and the
MCP snapshot, including the guarded-creation conditionals, is unchanged.

The operator command keeps options that MCP never had. The CLI `prepare`
table moves them into one `cli_options` object: `add`'s `--parent`,
`--dependencies`, `--plan`, `--ref`, `--status` and `--source-task`, and
`update`'s `--approve`, `--force`, `--discard-candidate` and `--artifact`. The
tool registry admits `cli_options` only from a caller without an MCP session,
so an MCP call that sends it gets the same `unknown field 'cli_options'` as
any other unknown field. A request carrying it is screened as the operator
command was (`TaskWriteScreen::from_process`, bound to a reservation owner
when there is one) and keeps the CLI's update semantics: `--approve` takes
the approval step, and other updates are lifecycle or forced writes without
the guarded start and approval transitions. A claimed worker's `add` refuses
`cli_options`.

`--host` routing for `update` is unchanged: the preflight sends the derived
input to the owning host as `orbit.task.update`, refusing the flags with no
tool field, as before. The retired-field and unknown-field refusals the tools
shipped are spec entries (§2.1, §4), and over MCP and `orbit tool run` the
registry's schema check still answers first with the same plain `unknown
field` message. The Step 0 baseline and its differences are in
[Migrated CLI verbs execute through the tool path](./4_decisions.md#migrated-cli-verbs-execute-through-the-tool-path).

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
- [ORB-15933] — phase 0b: built input validation at the tool entry in warn
  mode, with aliases, retired fields and `at_least_one_of`.
- [ORB-15934] — phase 0b: unknown-field refusal after the compatibility window.
- [ORB-15935] — phase 1: migrated auto-task and built the switch, enum, object
  and CLI-only parameter kinds.
- [ORB-15936] — phase 2, drain and workspace claim.
- [ORB-15937] — phase 3a: migrated the task read verbs and settled ID routing.
- [ORB-15938] — phase 3b: migrated task `add` and `update` with the guarded
  schema extension.
- [ORB-15939], [ORB-15940] — phase 3, the rest of the task noun.
- [ORB-15941] — phase 4, workflow run.
- [ORB-15942] — phase 5, routine control and search.
- [ORB-15943] — verifies the authorization golden still catches placement and
  permission drift.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

---
title: Operations as Data — Migration Cookbook
owner: claude
last_updated: 2026-10-13
last_validated: 2026-10-12
status: Accepted
feature: operations-as-data
doc_role: reference
type: design
summary: Step-by-step procedure for migrating a noun to the operation registry under kernel v2, and for adding a verb to an already-migrated noun.
tags: [operations-as-data, architecture, adr-0209, cookbook]
paths: ["crates/orbit-common/src/governance/friction/**", "crates/orbit-common/src/governance/auto_task/**", "crates/orbit-cli/src/command/operation/args.rs", "crates/orbit-common/src/governance/task/**", "crates/orbit-cli/src/command/task/operation.rs"]
related_features: [operations-as-data]
related_artifacts: [ORB-10358, ORB-15932, ORB-15933, ORB-15935, ORB-15936, ORB-15937, ORB-15938]
---

# Operations as Data — Migration Cookbook

Written from the friction migration ([ORB-10358]) and updated for the accepted
kernel v2 in [2_design.md](../2_design.md). Phase 0a of
[2_design.md §9](../2_design.md#9-migration-plan) built kernel v2's structure
([ORB-15932]): `NounSpec`, `cli_path`, `effect`, parameter groups, `redact`,
the `operation_registry!` macro and the generic adapters. Phase 0b built input
validation at the tool entry in warn mode ([ORB-15933]): `parse_input`,
`aliases`, `at_least_one_of` and `retired_fields`. Phase 1 (auto-task,
[ORB-15935]) built the `Switch`, `Enum`, `Object` and CLI-only parameter
kinds, `default`, and flag aliases and value names. Phase 3a (task reads,
[ORB-15937]) built `EnumList`, multi-value flags, optional positionals and
`CliHelp` (long description, after-help examples, flag order), and showed how
to migrate part of a noun. Phase 3b (task writes, [ORB-15938]) built `Array`,
CLI-required flags (`cli_required`), `UnknownFields::Refuse` for a verb that
already refused extras, retired-field wordings, the `GuardedModes::preempt`
and `schema_extension` tables, and a hidden option object for operator-only
CLI flags. Phase 3c (task artifacts and locks, [ORB-15939]) built two-segment `cli_path`
nesting, `cli_groups` help and inherited group arguments, and CLI argument
relationships. A noun needing another kind the kernel lacks extends the kernel
first. Two recipes: migrating a noun that is still hand-wired, and
adding a verb to a noun that is already migrated.

---

## Recipe A — migrate a noun

### Step 0. Freeze the surface and the behaviour you must not move

**Do this before touching any code.** Build the current binary and capture the
help output for the parent command and every subcommand, including nested ones:

```sh
cargo build -p orbit-cli --bin orbit
mkdir -p .orbit/tmp
for sub in "" add list show …; do
  ./target/debug/orbit <noun> $sub --help > .orbit/tmp/<noun>-help-${sub:-root}.txt 2>&1
done
```

`crates/orbit-cli/tests/snapshots/mcp_tools_list.json` is already in-tree, so
`git diff` on it at the end is the MCP proof.

If the noun's CLI calls core services directly today (task, auto-task, routine,
workflow run), the help goldens are not enough: moving the CLI onto the tool
path changes behaviour ([Migrated CLI verbs execute through the tool path](../4_decisions.md#migrated-cli-verbs-execute-through-the-tool-path)).
Also record, per verb, from the pre-migration binary:

- the `--json` output for a representative call;
- the audit rows one call writes (`command`, `subcommand`, target);
- the outcome for a non-interactive caller with no `ORBIT_OPERATOR` and no agent
  envelope, which the capability chokepoint may now refuse;
- the attribution the stored record carries.

These captures are the only reliable evidence that a derived surface did not
move. Reconstructing them after the migration proves nothing.

### Step 1. Inventory the existing declaration sites

For a noun with verbs `v₁…vₙ`, find:

| What | Where (friction example) |
|------|--------------------------|
| MCP `Tool` impls and `input_schema()` overrides | `crates/orbit-tools/src/builtin/orbit/<noun>/*.rs` |
| MCP registration + workspace scope | `crates/orbit-tools/src/builtin/orbit/register.rs` |
| Action enum variants | `crates/orbit-tools/src/host.rs` (`OrbitBuiltinAction`) |
| Handler dispatch | `crates/orbit-core/src/adapter/tool_host/dispatch.rs` |
| Handlers, and the input names and aliases they read | `crates/orbit-core/src/adapter/tool_host/<noun>_tools.rs` |
| Redaction policy | `crates/orbit-core/src/adapter/tool_host/artifact_redaction.rs` |
| Claimed-worker routing | `crates/orbit-core/src/adapter/tool_host/worker_tools.rs` |
| CLI args + `Execute` impls | `crates/orbit-cli/src/command/<noun>/` |
| CLI audit metadata and runtime need | `crates/orbit-cli/src/command/operation/registry.rs` |
| Governed operations | `crates/orbit-common/src/governance/authorization/catalog.rs` |
| Web handlers | `crates/orbit-web/src/api/<noun>s.rs` |

Note where the *same* field is described differently in two places. Do **not**
harmonize the wording during the migration. Model the difference in the spec
(`mcp_description` versus `cli.help`) and change it, if at all, in a later PR
that is honestly a wire change.

Note every spelling a handler accepts beyond the declared name (`description`
for `body`, `task_id` for `during_task`). Each one becomes a declared alias.

### Step 2. Write the noun spec, the verb enum and the registry

In `orbit-common`, next to the noun's existing shared types. Invoke
`operation_registry!` with one `Variant => SPEC` list: it emits the verb enum,
its `OperationVerb` impl (an exhaustive `spec()` match) and the ordered
operations list, so they cannot disagree. Write a `NounSpec` const holding the
CLI name, tool prefix, `--help` summary, audit target type, the parameter whose
value is the audit target id, any intermediate CLI groups, and the operations
list. `friction/operations.rs` and `auto_task/operations.rs` are the examples.

For each verb, fill `cli_path`, `effect` and its parameter groups. For each
parameter, fill `redact` in addition to the friction-era fields: `FreeText` for
prose a caller writes, `Path` for a path or URL, `None` otherwise. Also fill
`aliases` with every alternative spelling the handlers accept today,
`retired_fields` with every field they refuse with a pointer to its
replacement, `at_least_one_of` with any "change something" rule, and `default`
with any `default_value` the old clap struct declared.
Choose `effect` from what the verb does, not from what the old CLI arm happened
to open; where they differ, say so in the PR, with evidence that a verb moving
to the read-only runtime does not rely on stale-run reconciliation on open.

Declaration order is contract twice over: it is `--help` subcommand order, and
within a spec, parameter order is both `--help` arg order and MCP schema order.
Copy both orders from the code you are replacing.

Factor repeated shapes with `const fn` helpers (friction has `text_param` and
`count_param`) and shared flags with a parameter group, but write out anything
that differs. A helper that needs three override arguments is not a helper.

Where the CLI and MCP shapes differ — one flag per field on the CLI, one
structured object over MCP — declare the MCP field without a CLI binding and
each flag as a **CLI-only** parameter (a binding and no MCP description). A
CLI-only parameter is not a wire field: the CLI's `prepare` table consumes it
and composes the structured field (auto-task's `--cron`/`--every-minutes`/
`--deliveries-landed` → `schedule`). A wire field the CLI composes and MCP does
not advertise has neither binding (auto-task `update`'s `template_patch`).

### Step 3. Test the registry at the boundary

Before wiring any surface, check what the compiler cannot:

- tool names lie beneath `tool_prefix` and are unique (drain uses `orbit`
  to retain `orbit.task.pull` alongside `orbit.drain.*`);
- parameter names and aliases are unique within each operation, and no alias
  equals another parameter's name;
- subcommand order matches the shipped `--help` order (the Step 0 goldens);
- MCP exposure matches `docs/design/mcp-bridge/references/conformance-v1.yaml`.

Exercise the derived public surface at the boundary, per the test strategy;
the friction registry's original unit tests were retired. The verb enum, the
`spec()` match and the operations list agree by construction, so there is no
test for that.

### Step 4. Collapse the action enum

Replace the noun's `n` `OrbitBuiltinAction` variants with one
`<Noun>(<Noun>Verb)` and add `From<<Noun>Verb> for OrbitBuiltinAction`. Fix the
fallout mechanically:

- `OrbitBuiltinAction::<Noun>Add` → `OrbitBuiltinAction::<Noun>(<Noun>Verb::Add)`;
- collapse or-patterns: `<Noun>(<Noun>Verb::Add | <Noun>Verb::Update)`;
- turn every wildcard arm that covers the noun into an exhaustive `match` on
  the verb, so a future verb has to state its behaviour. Under
  [The spec holds cross-surface facts; single-surface facts are an exhaustive match in that surface](../4_decisions.md#the-spec-holds-cross-surface-facts-single-surface-facts-are-an-exhaustive-match-in-that-surface)
  this includes `worker_tools.rs` and `artifact_redaction.rs`. The redaction
  field lists come from the parameters' `redact` class: give the noun's arm in
  `policy_for_action` its spec's `params` and delete its hand-written lists. A
  declared alias takes its parameter's class, so an accepted spelling is
  redacted once it is in `aliases` (friction `add`'s `description`).

### Step 5. Derive MCP

Delete the per-verb `Tool` files. Register the noun with
`operation::register_noun(registry, &NOUN)`, which serves every operation with
the generic `OperationTool<V>`; it takes `execution_kind` from `effect` and
dispatches through the `From<<Noun>Verb> for OrbitBuiltinAction` impl from
Step 4. It also validates input with `parse_input` and reports unknown fields
(a `warn!` and one audit row each) before dispatch.
Delete the noun's hand-written registration block. For a verb without MCP
scope, `NounSpec::tool_only_registration` selects active local registration
or inactive registration; preserve the previous kind. MCP-scoped verbs still
use `register_mcp`.

If a verb overrode `input_schema()`, move the override into the noun's exhaustive
`schema_extension(verb)` table in `orbit-tools`. Allowed values and defaults stay
in the parameter's MCP description; the schema emits no `enum` or `default`
keyword ([2_design.md §6](../2_design.md#6-mcp-adapter)).

If a tool already refused unknown fields, declare `unknown_fields: Refuse` so
the migration keeps refusing them; every other verb starts in `Warn`. Declare
each retired field with the `RetiredField` constructor whose wording matches
the shipped refusal.

If the CLI command has options MCP must not accept (task `add --plan`,
`update --approve`), the CLI `prepare` table moves them into one hidden
object parameter (task's `cli_options`) for the core handler to read. Admit
that field only from a caller without an MCP session, so MCP keeps refusing
it.

If a tool routed some inputs elsewhere before dispatch (auto-task's bounded
`list`, acknowledged `mint` and checked `update` go to desktop host actions),
implement the noun's `GuardedModes` table in `orbit-tools`, an exhaustive match
on the verb that returns `None` for every verb without such a mode. Every noun
implements it; friction's returns `None` throughout. A mode whose input the
flat spec does not describe (task's guarded creation and update) goes in
`preempt`, which runs before `parse_input`.

For versioned inputs whose protocol-skew refusal must precede shape validation,
use the host's `prepare_operation_input` hook before `parse_input` (drain pull).

Unknown-field refusal starts in warn-and-audit mode for the noun; the refusal
flip is a separate change after the 30-day compatibility window.

Then assert the derived schemas equal the shipped ones — name, description,
parameter order, types, requiredness and exact description strings — through
`crates/orbit-tools/tests/tools/mcp_definitions.rs` and the
`mcp_tools_list.json` snapshot.

### Step 6. Derive the CLI

Delete the per-verb `Args` structs and `Execute` impls. The generic
`Invocation<V>` supplies the clap impls; the parent command's subcommand field
has type `Invocation<<Noun>Verb>`, and its `#[command(about)]` reads the noun
spec's `cli_about`. Keep only the noun's exhaustive CLI surface tables:
`render(verb, &Value)` and, if the noun shapes or checks requests for the CLI
only, `prepare(verb, &mut Map)`. `prepare` removes every CLI-only parameter it
consumes, so none reaches the tool as an unknown field; a client-side filter
(auto-task `list --enabled`) is returned from `prepare` to `render`.

Every verb executes through `run_tool`, including CLI-only verbs registered with
`mcp_scope: None`. A destructive CLI-only verb becomes reachable through
`orbit tool run`, so give it a `GOVERNED_OPERATIONS` row in this PR and re-bless
the authorization matrix golden in `crates/orbit-cli/tests/output/output_goldens.rs`
(`make goldens UPDATE=1`), reviewing its diff.

Replace the noun's arm in `command/operation/registry.rs` with the generic
helper `registry_operation` (runtime need from `effect`, audit metadata from the
noun spec and `cli_path`). Keep any input-dependent override, such as `governed_when` or a
flag that needs the full runtime, around the helper, and check the derived audit
labels against the old arm.

**Migrating part of a noun.** When only some verbs move (task reads), keep
the hand-wired subcommands and add one flattened variant,
`#[command(flatten)] Operation(Invocation<<Noun>Verb>)`, to the existing
subcommand enum. Comment it with `//`: a `///` doc comment on a flattened
variant becomes help text and moves the goldens. The noun's registry arm
matches that variant first and returns the generic helper (task wraps it as
`task_operation`); the hand-wired arms stay below it. Facts the old arm held
that are not in the spec, such as owner-workspace binding, come from the
shared tables that already hold them, not from a new per-verb override.

Now cash in Step 0: commit the captured help files as
`crates/orbit-cli/tests/help_goldens/<noun>/*.txt` and add each argv to `CASES`
in `crates/orbit-cli/tests/output/help_goldens.rs`.

### Step 7. Derive the handler table

Add `dispatch(runtime, verb, input, model)` to `<noun>_tools.rs` with one
exhaustive `match` on the verb, make the per-verb handlers private to it, and
reduce `dispatch.rs` to a single arm. Handlers take `OperationInput` and read
canonical names only; delete their alias lists and any presence or type check
`parse_input` performs. Keep content checks. Delete any CLI-side restatement of
`at_least_one_of`: the CLI adapter's `Invocation::require_at_least_one_of`
words the refusal in flags.

### Step 8. Point the dashboard at the registry

Take tool names from `<Noun>Verb::tool_name()` (an `OperationVerb` method).
Leave route shapes, serde bodies and HTTP-specific defaults alone — they are not
verb data, except that a serde body must forward the fields its verb does not
declare (a flattened `extra` map passed through `forward_undeclared`), or the
tool entry never sees them. Once the tool entry refuses undeclared fields, the
dashboard needs no field-name check of its own.

### Step 9. Verify contract stability

Run the gates in [CLAUDE.md](../../../../CLAUDE.md#gates) (`make ci-fast`,
`make ci-test-affected`, `make ci-lint`, `make goldens`), sequentially, then:

```sh
git diff --stat crates/orbit-cli/tests/snapshots/mcp_tools_list.json   # empty, or explained
diff -ru .orbit/tmp/<noun>-help-before .orbit/tmp/<noun>-help-after                # must be empty
```

An empty MCP snapshot diff plus an empty help diff is what "wire compatible"
means. Compare the Step 0 behaviour captures as well. Every difference is a
consumer-visible change that the PR description lists.

### Step 10. Record the outcome

Update this feature's docs: move the noun's rows in [2_design.md](../2_design.md)
from Planned to Implemented, update the §9 progress counts, and add the noun to
[1_overview.md](../1_overview.md)'s At a Glance table. If the migration
surfaced a cost the decisions do not already name, name it in
[4_decisions.md](../4_decisions.md).

---

## Recipe B — add a verb to a migrated noun

1. Add the verb to the noun's `operation_registry!` list with its spec, at the
   position it should take in `--help`. Compilation now fails in every surface
   table for the noun.
2. Fill each failing table: the handler, the CLI renderer and request shaping,
   and any worker route or redaction decision the compiler asks for.
3. If it destroys something outside its run's purpose, add its
   `GOVERNED_OPERATIONS` row and re-bless the authorization matrix golden.
4. Add a route only if the verb should be reachable over HTTP.

No adapter file is edited: the subcommand, its flags, its `--help`, its tool
schema, its MCP placement, its execution kind, its audit metadata and its
runtime need all come from the spec.

---

## Gotchas the friction migration hit

- **clap's help ordering is insertion order for flags and a separate sequence for
  positionals.** Reproducing derive's output means adding args in field order and
  letting positionals fall where they fall. Freezing the help fixtures first is
  what makes this checkable.
- **Required and optional inputs were trimmed differently** by the old
  hand-written code, and that difference is observable in error messages.
  Preserve it; do not clean it up mid-migration.
- **`register_inactive` tools are still executable** through `run_tool`; they
  are hidden from MCP, not disabled. `ToolRegistry::schemas()` filters to active
  tools, so use `has()` when asserting that every verb is registered.
- **Functional record update (`..OTHER`) is not usable in a `const`.** Write
  near-duplicate specs out, factor a `const fn`, or use a parameter group.
- **`value_name` does not need interning.** The workspace enables clap's
  `string` feature, so the adapter passes the owned SCREAMING_SNAKE `String`.
  The friction pilot `Box::leak`ed it before anyone noticed.
- **A baseline binary must build its own fixture before comparing audit
  rows.** A binary that joins a store generation another binary created
  skips its CLI audit row, so an old-versus-new run on one shared fixture
  shows the new binary writing no audit at all. Compare output on the shared
  fixture and audit rows on a fixture each binary built (task reads,
  [ORB-15937]).
- **Run in-process CLI tests outside a managed run's environment.** A
  delivery shell carries the managed-worker variables (`ORBIT_MANAGED_RUN_CONTEXT`,
  `ORBIT_WORKER_CONTEXT_REQUIRED` and the task and run IDs), and tests that
  open a runtime refuse with "managed worker runtime binding unavailable".
  Unset them for focused local runs.
- **Test layout.** Sibling `tests/` mirroring source filenames, per
  [test_layout.md](../../../design-patterns/test_layout.md), not a nested
  `<module>/tests.rs` child.

## Task References

- [ORB-10358] — the friction migration this cookbook is drawn from.
- [ORB-15932] — built kernel v2's structure on friction.
- [ORB-15933] — built input validation at the tool entry on friction, in warn
  mode.
- [ORB-15935] — migrated auto-task, building the switch, enum, object and
  CLI-only parameter kinds.
- [ORB-15937] — migrated the task read verbs, building enum lists, multi-value
  flags, optional positionals and `CliHelp`, and the part-migrated noun shape.
- [ORB-15938] — migrated task `add` and `update`, building arrays,
  CLI-required flags, refuse-mode unknown fields, `preempt`,
  `schema_extension` and the hidden CLI option object.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

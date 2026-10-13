---
title: Operations as Data — Migration Cookbook
owner: claude
last_updated: 2026-10-12
last_validated: 2026-10-12
status: Accepted
feature: operations-as-data
doc_role: reference
type: design
summary: Step-by-step procedure for migrating a noun to the operation registry under kernel v2, and for adding a verb to an already-migrated noun.
tags: [operations-as-data, architecture, adr-0209, cookbook]
paths: ["crates/orbit-common/src/governance/friction/**", "crates/orbit-cli/src/command/operation/args.rs"]
related_features: [operations-as-data]
related_artifacts: [ORB-10358]
---

# Operations as Data — Migration Cookbook

Written from the friction migration ([ORB-10358]) and updated for the accepted
kernel v2 in [2_design.md](../2_design.md). Steps that need kernel v2 say so;
until phase 0 of [2_design.md §9](../2_design.md#9-migration-plan) lands, only
the friction-era kernel exists. Two recipes: migrating a noun that is still
hand-wired, and adding a verb to a noun that is already migrated.

---

## Recipe A — migrate a noun

### Step 0. Freeze the surface and the behaviour you must not move

**Do this before touching any code.** Build the current binary and capture the
help output for the parent command and every subcommand, including nested ones:

```sh
cargo build -p orbit-cli --bin orbit
for sub in "" add list show …; do
  ./target/debug/orbit <noun> $sub --help > /tmp/<noun>-help-${sub:-root}.txt 2>&1
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

In `orbit-common`, next to the noun's existing shared types. Under kernel v2,
the per-noun declarative macro emits the verb enum, its `spec()` match and the
ordered operations list from one list, so they cannot disagree. Write a
`NounSpec` holding the CLI name, tool prefix, `--help` summary, audit target
type, any intermediate CLI groups, and the operations.

For each verb, fill `cli_path`, `effect`, parameter groups,
`at_least_one_of` and `retired_fields`. For each parameter, fill `aliases`,
`default` and `redact` in addition to the friction-era fields. Choose `effect`
from what the verb does, not from what the old CLI arm happened to open; where
they differ, say so in the PR.

Declaration order is contract twice over: it is `--help` subcommand order, and
within a spec, parameter order is both `--help` arg order and MCP schema order.
Copy both orders from the code you are replacing.

Factor repeated shapes with `const fn` helpers (friction has `text_param` and
`count_param`) and shared flags with a parameter group, but write out anything
that differs. A helper that needs three override arguments is not a helper.

### Step 3. Test the registry at the boundary

Before wiring any surface, check what the compiler cannot:

- tool names follow `<tool_prefix>.<verb>` and are unique;
- parameter names and aliases are unique within each operation, and no alias
  equals another parameter's name;
- subcommand order matches the shipped `--help` order (the Step 0 goldens);
- MCP exposure matches `docs/design/mcp-bridge/references/conformance-v1.yaml`.

Exercise the derived public surface at the boundary, per the test strategy;
the friction registry's original unit tests were retired.

### Step 4. Collapse the action enum

Replace the noun's `n` `OrbitBuiltinAction` variants with one
`<Noun>(<Noun>Verb)` and add `From<<Noun>Verb> for OrbitBuiltinAction`. Fix the
fallout mechanically:

- `OrbitBuiltinAction::<Noun>Add` → `OrbitBuiltinAction::<Noun>(<Noun>Verb::Add)`;
- collapse or-patterns: `<Noun>(<Noun>Verb::Add | <Noun>Verb::Update)`;
- turn every wildcard arm that covers the noun into an exhaustive `match` on
  the verb, so a future verb has to state its behaviour. Under
  [The spec holds cross-surface facts; single-surface facts are an exhaustive match in that surface](../4_decisions.md#the-spec-holds-cross-surface-facts-single-surface-facts-are-an-exhaustive-match-in-that-surface)
  this includes `worker_tools.rs` and `artifact_redaction.rs`. Under kernel v2
  the redaction field lists come from the parameters' `redact` class; delete the
  noun's hand-written lists.

### Step 5. Derive MCP

Delete the per-verb `Tool` files. Under kernel v2, register the noun's
operations with the generic `OperationTool<V>`; it validates input with
`parse_input`, takes `execution_kind` from `effect`, and dispatches the action.
Delete the noun's block from `register_builtins`.

If a verb overrode `input_schema()`, move the override into the noun's exhaustive
`schema_extension(verb)` table in `orbit-tools`. If a parameter has allowed values
or a default, decide whether its schema keeps them in the description
(wire-identical) or emits `enum`/`default` (additive wire change), and say which
in the PR.

Unknown-field refusal starts in warn-and-audit mode for the noun; the refusal
flip is a separate change after the 30-day compatibility window.

Then assert the derived schemas equal the shipped ones — name, description,
parameter order, types, requiredness and exact description strings — through
`crates/orbit-tools/tests/tools/mcp_definitions.rs` and the
`mcp_tools_list.json` snapshot.

### Step 6. Derive the CLI

Delete the per-verb `Args` structs and `Execute` impls. Under kernel v2 the
generic `Invocation<V>` supplies the clap impls; the parent command's subcommand
field has type `Invocation<<Noun>Verb>`. Keep only the noun's exhaustive CLI
surface tables: `render(verb, &Value)` and, if the noun shapes requests for the
CLI only, `prepare(verb, &mut Map)`.

Every verb executes through `run_tool`, including CLI-only verbs registered with
`mcp_scope: None`. A destructive CLI-only verb becomes reachable through
`orbit tool run`, so give it a `GOVERNED_OPERATIONS` row in this PR and re-bless
the authorization matrix golden in `crates/orbit-cli/tests/output/output_goldens.rs`
(`make goldens UPDATE=1`), reviewing its diff.

Replace the noun's arm in `command/operation/registry.rs` with the generic
helper (runtime need from `effect`, audit metadata from the noun spec and
`cli_path`). Keep any input-dependent override, such as `governed_when` or a
flag that needs the full runtime, around the helper, and check the derived audit
labels against the old arm.

Now cash in Step 0: commit the captured help files as
`crates/orbit-cli/tests/help_goldens/<noun>/*.txt` and add each argv to `CASES`
in `crates/orbit-cli/tests/output/help_goldens.rs`.

### Step 7. Derive the handler table

Add `dispatch(runtime, verb, input, model)` to `<noun>_tools.rs` with one
exhaustive `match` on the verb, make the per-verb handlers private to it, and
reduce `dispatch.rs` to a single arm. Under kernel v2, handlers take
`OperationInput` and read canonical names only; delete their alias lists and
any presence check `parse_input` now performs. Keep content checks.

### Step 8. Point the dashboard at the registry

Take tool names from `<Noun>Verb::tool_name()`. Leave route shapes, serde bodies
and HTTP-specific defaults alone — they are not verb data. The tool entry
refuses undeclared fields, so the dashboard needs no field-name check of its
own.

### Step 9. Verify contract stability

Run the gates in [CLAUDE.md](../../../../CLAUDE.md#gates) (`make ci-fast`,
`make ci-test-affected`, `make ci-lint`, `make goldens`), sequentially, then:

```sh
git diff --stat crates/orbit-cli/tests/snapshots/mcp_tools_list.json   # empty, or explained
diff -ru /tmp/<noun>-help-before /tmp/<noun>-help-after                # must be empty
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

1. Add the verb to the noun's macro list with its spec. Compilation now fails in
   every surface table for the noun.
2. Place it in the operations list where it should appear in `--help`.
3. Fill each failing table: the handler, the CLI renderer, and any schema
   extension, worker route or redaction decision the compiler asks for.
4. If it destroys something outside its run's purpose, add its
   `GOVERNED_OPERATIONS` row and re-bless the authorization matrix golden.
5. Add a route only if the verb should be reachable over HTTP.

No adapter file is edited: the subcommand, its flags, its `--help`, its tool
schema, its MCP placement, its validation, its audit metadata and its runtime
need all come from the spec.

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
  `string` feature, so pass the owned SCREAMING_SNAKE `String`; the friction
  adapter's `Box::leak` predates noticing that.
- **Test layout.** Sibling `tests/` mirroring source filenames, per
  [test_layout.md](../../../design-patterns/test_layout.md), not a nested
  `<module>/tests.rs` child.

## Task References

- [ORB-10358] — the friction migration this cookbook is drawn from.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

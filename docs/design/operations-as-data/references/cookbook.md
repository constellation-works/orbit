---
title: Operations as Data — Migration Cookbook
owner: claude
last_updated: 2026-07-26
last_validated: 2026-08-31
status: Accepted
feature: operations-as-data
doc_role: reference
type: design
summary: Step-by-step procedure for migrating a noun to the operation registry, and for adding a verb to an already-migrated noun.
tags: [operations-as-data, architecture, adr-0209, cookbook]
paths: ["crates/orbit-common/src/governance/friction/**", "crates/orbit-cli/src/command/operation_args.rs"]
related_features: [operations-as-data]
related_artifacts: [ORB-10358]
---

# Operations as Data — Migration Cookbook

Written from the friction migration ([ORB-10358]), which is the worked example
for every step below. Two recipes: migrating a noun that is still hand-copied,
and adding a verb to a noun that is already migrated.

---

## Recipe A — migrate a noun

### Step 0. Freeze the surface you must not move

**Do this before touching any code.** Build the current binary and capture the
help output for the parent command and every subcommand:

```sh
cargo build -p orbit-cli --bin orbit
for sub in "" add list show …; do
  ./target/debug/orbit <noun> $sub --help > /tmp/<noun>-help-${sub:-root}.txt 2>&1
done
```

Then check `crates/orbit-cli/tests/snapshots/mcp_tools_list.json` into your
memory as the MCP baseline — it is already in-tree, so `git diff` on it at the
end is the proof.

These captures are the *only* reliable evidence that a derived surface did not
move. Reconstructing them after the migration proves nothing.

### Step 1. Inventory the existing declaration sites

For a noun with verbs `v₁…vₙ`, find:

| What | Where (friction example) |
|------|--------------------------|
| MCP `Tool` impls | `crates/orbit-tools/src/builtin/orbit/<noun>/*.rs` |
| MCP registration + workspace scope | `crates/orbit-tools/src/builtin/orbit/mod.rs` |
| Action enum variants | `crates/orbit-tools/src/lib.rs` (`OrbitBuiltinAction`) |
| Handler dispatch | `crates/orbit-core/src/adapter/tool_host/dispatch.rs` |
| Handlers | `crates/orbit-core/src/adapter/tool_host/<noun>_tools.rs` |
| CLI args + `Execute` impls | `crates/orbit-cli/src/command/<noun>.rs` |
| CLI audit metadata | `crates/orbit-cli/src/command/operation.rs` |
| Web handlers | `crates/orbit-web/src/api/<noun>s.rs` |

Note where the *same* field is described differently in two places. Do **not**
harmonize the wording during the migration — model the difference in the spec
(`mcp_description` vs `cli.help`) and change it, if at all, in a later PR that
is honestly a wire change.

### Step 2. Write the verb enum and the registry

In `orbit-common`, next to the noun's existing shared types:

```rust
pub enum <Noun>Verb { Add, List, /* … */ }
pub type <Noun>Operation = OperationSpec<<Noun>Verb>;

impl <Noun>Verb {
    pub fn spec(self) -> &'static <Noun>Operation {
        match self { <Noun>Verb::Add => &ADD, /* … */ }   // exhaustive on purpose
    }
}

pub const <NOUN>_OPERATIONS: &[<Noun>Operation] = &[ADD, LIST, /* … */];
```

Declaration order is contract twice over: it is `--help` subcommand order, and
within a spec, `params` order is both `--help` arg order and MCP schema order.
Copy both orders from the code you are replacing — the enum variant order for
subcommands, the struct field order for parameters.

Factor repeated shapes with `const fn` helpers (friction has `text_param` and
`count_param`), but write out anything that differs. A helper that needs three
override arguments is not a helper.

### Step 3. Write the registry invariant tests

Before wiring any surface. These are cheap and they catch authoring mistakes the
compiler cannot:

- every verb has exactly one spec, and `spec()` agrees with the table;
- verb names and tool names are unique and follow `orbit.<noun>.<verb>`;
- parameter names are unique within each operation;
- subcommand order matches the shipped `--help` order (assert the literal list);
- MCP exposure matches `docs/design/mcp-bridge/references/conformance-v1.yaml`.

See `crates/orbit-common/src/governance/friction/tests/mod.rs`.

### Step 4. Collapse the action enum

Replace the noun's `n` `OrbitBuiltinAction` variants with one
`<Noun>(<Noun>Verb)`. Fix the fallout mechanically:

- `OrbitBuiltinAction::<Noun>Add` → `OrbitBuiltinAction::<Noun>(<Noun>Verb::Add)`;
- collapse or-patterns: `<Noun>(<Noun>Verb::Add | <Noun>Verb::Update)`;
- turn any `_ => unreachable!()` inside a noun-scoped helper into an exhaustive
  `match` on the verb, so a future verb has to state its behavior. The friction
  hub executor did this and now returns a real error for `Stats`/`Resolve`
  instead of panicking on a case it never received.

### Step 5. Derive MCP

Delete the per-verb `Tool` files. Add one `<Noun>OperationTool(&'static
<Noun>Operation)` whose `schema()` is `operation_tool_schema(self.0)` and whose
`execute()` dispatches `OrbitBuiltinAction::<Noun>(self.0.verb)`. Register with a
loop over the registry through `register_operation`, and delete the noun's block
from `register_builtins`.

Then assert the derived schemas equal the shipped ones — name, description,
parameter order, types, requiredness, and the exact description strings. See
`crates/orbit-tools/src/builtin/orbit/friction/tests/derived_schema.rs`.

### Step 6. Derive the CLI

Delete the per-verb `Args` structs and `Execute` impls. Keep the parent
`#[derive(Args)]` struct, and give its subcommand field the type
`Invocation<<Noun>Verb>` with hand-written `Subcommand` and `FromArgMatches`
impls that are three lines each, delegating to `operation_args`. Keep the
response renderers — those are presentation and stay per-noun.

Update `command/operation.rs`'s arm for the noun to read
`invocation.spec.name`, `invocation.target_id()`, and `invocation.json` instead
of matching verb by verb.

Now cash in Step 0: commit the captured help files as
`crates/orbit-cli/src/command/tests/<noun>_help/*.txt` and assert against them
with `include_str!`. Rebuild the binary and `diff` its live `--help` against the
captures too — the test and the binary should both be silent.

### Step 7. Derive the handler table

Add `dispatch(runtime, verb, input, model)` to `<noun>_tools.rs` with one
exhaustive `match` on the verb, make the per-verb handlers private to it, and
reduce `dispatch.rs` to a single arm.

### Step 8. Point the dashboard at the registry

Take tool names from `<Noun>Verb::tool_name()` and validate field names against
the spec. Leave route shapes, serde bodies, and HTTP-specific defaults alone —
they are not verb data. Add a test asserting every parameter the dashboard sets
is one the registry declares.

### Step 9. Verify contract stability

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
make ci-fast
git diff --stat crates/orbit-cli/tests/snapshots/mcp_tools_list.json   # must be empty
diff -ru /tmp/<noun>-help-before /tmp/<noun>-help-after                # must be empty
```

An empty MCP snapshot diff plus an empty help diff is what "wire compatible"
means. Anything else is a consumer-visible change that belongs in the PR
description.

### Step 10. Record the outcome

Append a pilot/migration note directly to [North-star architecture bearing: operations as data behind an operation registry](../../orbit-core/4_decisions.md#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry), citing the task that supplied the evidence.
(never hand-edit `.orbit/`), and add the noun to this feature's docs. If the
migration surfaced a cost the ADR does not already name, name it.

---

## Recipe B — add a verb to a migrated noun

1. Add the `<Noun>Verb` variant. Compilation now fails in exactly two places.
2. Add the spec `const` and list it in `<NOUN>_OPERATIONS`, in the position you
   want it to appear in `--help`.
3. Add the `spec()` arm.
4. Add the handler and its `dispatch` arm.
5. Add a route only if the verb should be reachable over HTTP.

Steps 1–4 are the whole cost for CLI and MCP. No surface file is edited: the
subcommand, its flags, its `--help`, its tool schema, its MCP exposure, its audit
metadata, and its JSON input projection all fall out of the spec. That claim is
executable — `crates/orbit-cli/src/command/tests/operation_args.rs` declares a
synthetic noun and asserts a complete working command line falls out of nothing
but a registry entry.

---

## Gotchas the friction migration hit

- **`clap::Arg::value_name` takes `&'static str` only.** Building a clap surface
  from runtime data needs interning; the adapter `Box::leak`s the SCREAMING_SNAKE
  form. Bounded, but know it is there.
- **clap's help ordering is insertion order for flags and a separate sequence for
  positionals.** Reproducing derive's output means adding args in field order and
  letting positionals fall where they fall. Freezing the help fixtures first is
  what makes this checkable instead of hopeful.
- **Required vs optional inputs were trimmed differently** by the old hand-written
  code, and that difference is observable in error messages. Preserve it; do not
  "clean it up" mid-migration.
- **`register_inactive` tools are still executable** through `run_tool` — they are
  hidden from MCP, not disabled. `ToolRegistry::schemas()` filters to active
  tools, so use `has()` when asserting that every verb is registered.
- **Functional record update (`..OTHER`) is not usable in a `const`.** Write
  near-duplicate specs out, or factor a `const fn` that takes the differing
  fields.
- **Test layout.** Sibling `tests/` mirroring source filenames, per
  [test_layout.md](../../../design-patterns/test_layout.md) — not a nested
  `<module>/tests.rs` child.

## Task References

- [ORB-10358] — the friction migration this cookbook is drawn from.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

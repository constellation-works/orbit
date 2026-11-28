# Authoring a task: modification footprint

Contents:

- [Modification footprint](#modification-footprint)
  - [Selector granularity](#selector-granularity)
  - [When to skip the pilot](#when-to-skip-the-pilot)

## Modification footprint

`context_files` is the task's lock footprint: list only likely creation,
modification and deletion targets that are a substantive part of the change
inside this workspace using `file:`, `dir:` or `symbol:path#name:kind`. Omit
read-only files, tests and fixtures only consulted, and shared reference docs
that at most need an incidental line update.

An entry matching
`workflow.unlocked_context_paths` (default every Markdown file) stays in the
task's context but takes no lock, unless every entry matches; the implementer
must still update affected docs in the same PR, and auto-fix resolves textual
merges of incidental edits.

Put supporting evidence paths
in the description instead of reserving locks for them. Prefer
precise verified files/symbols; a directory is appropriate for a genuinely owned
area, not a shortcut for all possibly relevant code.

### Selector granularity

Declare at symbol level when you can, so Orbit learns which symbols changes
and regressions concentrate in. Locks stay per file: a `symbol:` selector
locks its whole file, so granularity changes the record, not admission.

- Use `symbol:<path>#<name>:<kind>` for each existing source symbol the
  change modifies or deletes. `<name>` is the identifier as declared,
  qualified by its enclosing type or impl for a member (`Store::claim`);
  `<kind>` is one of `function`, `method`, `struct`, `enum`, `trait`,
  `impl`, `class`, `interface`, `type`, `const`, `static`, `macro`,
  `module` or `test`. The name must occur in the file at the revision you
  scoped.
- Use `file:` instead when the change is file-wide: a new file, new
  top-level items, imports or module wiring, a large refactor or move, more
  than about five symbols in the file, or a file with no symbols —
  Markdown, configuration, manifests, lockfiles, generated output, data and
  goldens.
- Never declare a symbol you expect to create; declare its file. Mixing is
  fine: `symbol:` selectors for the edited functions and `file:` for a file
  the change rewrites.

For substantive source targets, inspect the repository's generator or
synchronization evidence at the revision being scoped. Declare each exact
generated companion that the planned source change is expected to alter
alongside its canonical source, and record the source-to-output mapping and
generator evidence in the description.

Edit canonical sources and regenerate
through the repository's generator; declaring a companion does not authorize
hand-editing derived files. Do not sweep a generated tree, add unrelated
goldens, or attach outputs of read-only evidence sources. Add no companion
without evidence that it will change.

Existing selector checks and creation
authorization still apply: use the supported creation-intent path below for a
known missing companion; a generator relationship alone grants no creation
intent. Task-pilot records an expected missing output without authorization
as an evidence gap rather than proposing an undeclared missing selector.

If the workspace documents a generated unit-test identity baseline, follow its
inventory scope. When a plan adds, renames, moves or retires a test function
within that scope, declare the exact baseline as a `file:` companion and cite
the planned test and ratchet in the description or authoring evidence.

Regenerate it with the workspace's documented command, never by hand, and raise
it only for tests admitted by its test strategy. Tests outside that scope, tests
only read or run, body-only edits and plans with no unit-test identity change do
not trigger this declaration.

Follow a planned source change into any exact existing handwritten
expected-value fixture it necessarily invalidates: a hand-maintained
inventory, table or fixture whose asserted values the change must extend or
alter (for example, an independently handwritten expected inventory that a
new registry entry extends).

Declare only that exact existing fixture, cite
its pinned source evidence (`path:line` at the revision being scoped), and
record the source-to-expectation dependency in the description or authoring
evidence.

Such a fixture is neither generated output nor a test only
consulted or run: do not declare fixtures that are merely read, executed or
unchanged, do not reserve every affected test or a whole test directory, and
do not derive the expectation dynamically from the source it guards, which
would remove its independent check.

A required fixture that does not yet
exist follows the creation-intent rules below; without that authorization,
name it as an evidence gap rather than declaring its missing selector, and
never invent a filename.

When the planned change establishes a new or renamed action, or a changed
cross-module interface, follow it through the existing registration wiring
it requires and declare the exact existing sites that source evidence shows
must change: the typed action catalog or registry declaration, any embedding
or asset registry that must resolve the new or renamed reference, the
dispatcher that routes typed actions to implementations, and module
declarations or re-exports that expose a changed interface.

Record each
wiring dependency and its source reason in the description or authoring
evidence. Declare only sites that must change: do not reserve every registry
or caller, and add no unchanged registry selector for an existing same-name
action-body-only change. A required wiring target that does not yet exist
follows the creation-intent rules below; without that authorization, name it
as an evidence gap rather than declaring its missing selector, and never
predict an unspecified new filename.

When the plan creates a new Rust source or test file in a directory whose
existing declaration file (`main.rs`, `mod.rs`, `lib.rs` or `<dir>.rs`)
must declare it with a `mod` statement, declare that existing file with
`file:` and record the reason in the description or authoring evidence.

The new file itself stays under the creation-authorization rule below; the
declaration file exists at the pinned revision, so declaring it does not
predict a new filename. Do not declare a declaration file for a file the
build discovers automatically, such as a crate-root `tests/*.rs` binary.
A body-only edit to an existing module adds no declaration selector.

For review-filed tasks, task-pilot keeps selectors the finding names as change
targets. It may omit a selector it verifies as read-only evidence or an
incidental shared-doc touch by recording its exact selector and inspection
reason in `context_evidence_dropped`. Apply validates each record against
omitted, resolving review selectors and saves it in the assessment audit;
for `selectors`, unexplained omissions remain in filed order as
`context_evidence_retained`.

A `verified_no_diff` review assessment validates
the same drop records against its prepared scope at the inspection revision,
then keeps its empty footprint. Drop reasons do not replace the existing
already-landed or invalid-finding proof.
A record cannot override a proposed change target or an operator-declared
creation target. Keep evidence references in the finding's description.

For a known new target, explain creation intent. A missing `file:` selector
whose parent exists is accepted with a warning; other missing targets require
`allow_missing_context`, which a rejection names. Either route records each
missing selector as durable creation intent for that exact
canonical selector (at most 32 per task), committed with the scope change.

Later writes may re-send a declared selector without the option, and
task-pilot keeps it as a target to create. Maintained task writes carry intent
forward for kept selectors and revoke selectors removed from the replacement
list. The grant is bound to the task revision: an older writer that changes the
stored task without maintaining the grant invalidates it, even if it later
restores the same selector list.

Reauthorize through `orbit.task.update` with
the full `context_files` list plus `allow_missing_context: true`. Syntax,
containment and file/directory kind are still checked, and comments, plans or
agent proposals never grant intent. When task-pilot reports a missing selector
that carries no such record, re-declare it through that update path.

### When to skip the pilot

Add `no-pilot` when filing a task only if its `context_files` footprint is exact,
its complexity is assessed, its acceptance criteria are testable, and no dedupe
question remains open. Leave the tag off when any of these is a guess.

The tag
opts out of the automatic pilot, not readiness checks or diff delivery; an
operator can still request an explicit pilot. On a task a pilot has already
assessed, the tag also lifts pilot admission holds, so adding it then is an
operator's decision: an agent's update that adds it is refused.

Put read-only designs, conventions and examples in prose links. A design document
belongs in the footprint only if this task will change it. Cross-workspace edits
need separate tasks in their owning workspaces, with explicit dependencies when
one supplies the other; one managed worktree cannot deliver another repository.

# Code rules

The full text behind the `## Code` bullets of the root [`AGENTS.md`](../../AGENTS.md) that need more than a line.

- Names must be greppable. A `pub`/`pub(crate)` item used outside its module gets a name that says what it does and is near-unique in the repo (`role_styled_text`, not `text`; avoid bare `run`, `handle`, `process`, `new_state`). Never encode the file path in a name (`text_oc`); it lies after a move.
- Call such functions through their module (`use crate::output::common::color;` then `color::text(…)`), not `use …::{text}`, so `grep 'color::text('` finds every call site. Rename generic names when you touch them, not in bulk.
- User-facing CLI help and errors, MCP descriptions and UI text must stand alone: no Orbit source-tree paths or repo docs, only verified published docs or installed resources. Contributor docs may cite source paths; user-workspace paths like `.orbit/tmp/` are fine.
- Skill docs are canonical under `crates/orbit-core/assets/skills/`; after editing, run `scripts/sync-plugin-skills.sh` to regenerate `plugin/skills/`.

## Builtin MCP tool companions

When a plan adds, removes or renames a builtin MCP tool, or changes its
advertised placement, id route, description or input schema, inspect the
applicable companions at the revision being scoped (`rg -l` for the named test
or table and `rg --files` for a named golden). Declare an exact companion as a
`file:` selector only when its trigger below applies:

- The handwritten exact advertised-name list, asserted by
  `canonical_builtin_definitions_preserve_the_exact_workspace_surface`, changes
  with an advertised add, remove, rename or placement change. Its id-route
  tables, asserted by
  `every_builtin_id_route_is_declared_and_preserves_the_former_tables`, change
  with an id-route change. Declare the fixture for either change.
- The derived `authorization_matrix.json` golden, asserted by
  `authorization_matrix_matches_live_registry`, has one row per registered
  builtin and governed operation. Declare it when a builtin tool is added,
  removed or renamed; placement and id-route changes alone do not change it.
  Regenerate it with the documented golden command and review the diff; never
  hand-edit it.
- Inspect the handwritten `GOVERNED_TOOL_PLACEMENT` table, asserted against the
  live registry in the same test module, for each such plan. Declare its file
  only when the table must change: a placement change, or a governed tool
  operation added, removed or renamed.
- The claimed-worker routing match is in
  `crates/orbit-core/src/adapter/tool_host/worker_tools.rs`, routed by the
  exhaustive `worker_route` match. The file header notes a new tool family or
  verb must be placed there before the crate compiles. Declare this file when a
  plan adds, removes or renames a builtin `orbit.*` tool or verb. A placement or
  id-route change alone does not change it.

A body-only tool edit that keeps the name, verbs, placement and id route
declares none of those four companions.

The handwritten per-role MCP surface budget is `mcp_role_budgets.json` in the
`orbit-mcp` crate's `tests/boundary/` directory; `role_surface_budget` in
`role_surface.rs` reads it. Locate it at the revision being scoped with
`rg --files -g 'mcp_role_budgets.json'`, verify the matching include in
`role_surface.rs`, and declare its exact workspace-relative path as a `file:`
selector.

Its ceilings bound each role's tool count and approximate tokens (serialized
name, description and input-schema byte count divided by four). Declare this
fixture when a plan adds, removes or renames a builtin MCP tool, or changes its
advertised description or input schema. A placement-only change, id-route-only
change, or implementation-only edit that leaves the advertised name,
description and input schema unchanged does not trigger it. Keep the measured
surface within the recorded ceilings. If a change requires more surface, raise
only the affected ceiling in this file as a reviewed decision and explain in
the PR why the role needs the extra surface, as the file comment requires.

Resident tool rights, control actions and descriptions are defined in
`crates/orbit-common/src/governance/resident.rs`; include that source when a
change modifies a resident right, a control action or a `tool_description`. Any
MCP-visible change also affects the derived tool-list goldens in
`crates/orbit-cli/tests/output_goldens/tool_list.json` and
`crates/orbit-cli/tests/output_goldens/tool_list.plain.txt`. Include both and
regenerate them with `make goldens UPDATE=1`, never by hand.

## CodeQL helper selection

Before prescribing helper filenames, check them against exclusions in
`.github/codeql/codeql-config.yml`; production helpers must remain in CodeQL's
source selection.

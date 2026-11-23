# Code rules

The full text behind the `## Code` bullets of the root [`AGENTS.md`](../../AGENTS.md) that need more than a line.

- Names must be greppable. A `pub`/`pub(crate)` item used outside its module gets a name that says what it does and is near-unique in the repo (`role_styled_text`, not `text`; avoid bare `run`, `handle`, `process`, `new_state`). Never encode the file path in a name (`text_oc`); it lies after a move.
- Call such functions through their module (`use crate::output::color;` then `color::text(…)`), not `use …::{text}`, so `grep 'color::text('` finds every call site. Rename generic names when you touch them, not in bulk.
- User-facing CLI help and errors, MCP descriptions and UI text must stand alone: no Orbit source-tree paths or repo docs, only verified published docs or installed resources. Contributor docs may cite source paths; user-workspace paths like `.orbit/tmp/` are fine.
- Skill docs are canonical under `crates/orbit-core/assets/skills/`; after editing, run `scripts/sync-plugin-skills.sh` to regenerate `plugin/skills/`.

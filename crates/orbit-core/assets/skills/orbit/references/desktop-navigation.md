# Desktop navigation

When the connected client advertises `orbit_ui` (opt-in via `--toolset ui` or
`machine.mcp.toolsets` in the operator profile; included in the `agent` role),
use its read-only `open` and `inspect` actions to open the control center or a
selected task/run.
Discover workspaces first and copy the exact returned selector; pass `workspace`
and public `id` together, with `kind: run` for a run. Keep ordinary task tools
available when the client cannot render the UI.

The panel can capture proposed tasks, edit allowed fields, comment and record
reviews through separately guarded desktop tools. A click or conversation
reference grants no operator authority. Reread authoritative task state before
acting on a copied reference.

Keep the same request ID/payload when a write's
outcome is unknown; only a definite refusal permits a corrected fresh request.
Native support depends on the actual desktop build and connection; an installed
plugin or passing protocol test alone does not prove rendering/context delivery.

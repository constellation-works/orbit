---
type: context
summary: "This directory holds coverage records from QA passes."
last_validated: 2026-10-08
---

# QA evidence

This directory holds coverage records from QA passes. Each file maps a surface to the named tests and probes that demonstrated its behavior at the time of the pass, and lists the paths that pass left unproven. These files are records, not user or developer documentation, so they sit apart from the reference docs.

| File | Records |
|---|---|
| [cli-command-evidence.md](cli-command-evidence.md) | Which CLI command paths have an isolated, asserted process test, and which are excluded |
| [mcp-tool-evidence.md](mcp-tool-evidence.md) | The behavioral evidence for each advertised MCP tool, by transport, and its verified limits |
| [workflow-evidence.md](workflow-evidence.md) | The shipped jobs and the coverage boundaries of their named tests |
| [mcp-apps-probe.md](mcp-apps-probe.md) | The desktop control-center (MCP Apps) reproduction and native probe; desktop validation is still deferred |
| [mcp-apps-evidence-template.json](mcp-apps-evidence-template.json) | The evidence template `scripts/probe-mcp-apps.py` fills in |

Each record names the commit or date it was validated at. A record describes that point, not the current tree. Feature PRs do not need to keep these files current. A QA sweep that re-checks a surface refreshes its record and its validation date.

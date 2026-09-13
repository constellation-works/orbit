# orbit plugin evals

Eval suite for `claude plugin eval` (early access). Runs each case with the
plugin loaded and, by default, a no-plugin baseline arm; the score delta (Δ)
is what the plugin contributes.

All Orbit MCP calls are answered by mocks under `mocks/orbit/` — no real
`orbit mcp serve` process starts. `_tools.json` is the saved `tools/list` from
the real server so the mocked tools carry real schemas; refresh it after a
tool-surface change:

```bash
(printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"evals","version":"0"}}}' '{"jsonrpc":"2.0","method":"notifications/initialized"}' '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'; sleep 3) \
  | orbit mcp serve 2>/dev/null | grep '"id":2' \
  | python3 -c 'import json,sys; json.dump({"tools": json.load(sys.stdin)["result"]["tools"]}, open("evals/mocks/orbit/_tools.json","w"), indent=2)'
```

## Run

From the plugin root:

```bash
claude plugin eval . --trust-plugin            # full suite, both arms, 3 runs/case
claude plugin eval . --ablation none --runs 1  # cheap iteration on graders
claude plugin eval . --tag smoke               # just the smoke cases
claude plugin eval . --case orchestrate-backlog --judge-model sonnet
```

Cases:

| case | checks |
| --- | --- |
| `file-task` | `orbit` skill fires; `workspace_list` → `search` → `task_add` ordering; `task_add` carries `complexity`, `model`, `workspace`, `acceptance_criteria`; reply reports the new ID and doesn't claim dispatch |
| `task-status` | `task_show` by ID (no `task_list` scan); reply matches the record |
| `mark-done` | `in-progress → review → done` via at most two `task_update`s, each with `model` and a note; reply confirms done |
| `orchestrate-backlog` | `orbit-orchestrate` skill fires; `task_list` scoped to the workspace; no `task_update`/`workflow_ship`; report finds the DANI-41/42 overlap and the DANI-45 dependency |
| `off-topic-no-skill` | negative: no orbit skill, no orbit MCP call |

`results/` is gitignored.

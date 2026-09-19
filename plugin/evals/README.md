# orbit plugin evals

Eval suite for `claude plugin eval` (early access). Runs each case with the
plugin loaded and, by default, a no-plugin baseline arm; the score delta (Δ)
is what the plugin contributes.

All Orbit MCP calls are answered by mocks under `mocks/orbit/` — no real
`orbit mcp serve` process starts. Stateless tools (`workspace_list`, `crew_list`)
are fixed responders. Everything that depends on *which* record or workspace is
asked for, or on earlier calls — `task_add`/`task_list`/`task_show`/
`task_update`, `search`, frictions, runs, `workflow_ship`, `agent_invoke` — is
answered by one `type: agent` responder, `_server.md`, from a fixed fake world
(three workspaces, ~14 tasks, two frictions, three runs; new tasks get
ORB-9001 / DANI-9001) that also enforces the
lifecycle table (illegal transitions, missing plan, missing completion
evidence, `force`, unknown fields) and aborts the run on off-the-rails calls
such as an invented ID. Keep the world and the fixed mocks consistent when you
edit either. `_tools.json` is the saved `tools/list` from
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
claude plugin eval . --tag lifecycle             # the transition-table cases
```

`no-raw-cli`, `no-dot-orbit-edits` and `friction-resolve` list `Bash` (and
`Edit`/`Write`) in `allowed_tools` so the "don't" graders have something to
catch; pass `--allow-tools Bash Edit Write` if you want those calls to actually
execute rather than be refused (the graders count the attempt either way).
A `task-pilot-readonly` case was tried and dropped: the eval harness gives
subagents no MCP tools and no checkout to inspect, so the pilot can only
report "inconclusive".

Cases:

| case | checks |
| --- | --- |
| `file-task` | `orbit` skill fires; `workspace_list` before `task_add`; `task_add` carries `complexity`, `model`, `workspace`, `acceptance_criteria`; reply reports the new ID and doesn't claim dispatch. A direct human instruction — no search required (the bug it files, artifact path traversal, has no task in the world) |
| `task-status` | `task_show` by ID (no `task_list` scan); reply matches the record |
| `mark-done` | `in-progress → review → done` via at most two `task_update`s, each with `model` and a note; reply confirms done |
| `orchestrate-backlog` | `orbit-orchestrate` skill fires; `task_list` scoped to the workspace; no `task_update`/`workflow_ship`; report finds the DANI-41/42 overlap and the DANI-45 dependency |
| `off-topic-no-skill` | negative: no orbit skill, no orbit MCP call |
| `start-needs-plan` | `proposed → in-progress` on ORB-12300 sends a non-empty `plan` on the same `task_update`; `task_show` first; `model: claude` |
| `done-needs-evidence` | ORB-12310 (`review`) closes with an `execution_summary`; at most two updates |
| `regression-is-new-task` | ORB-11702 regressed: one `task_add` with `regression_from`, no status change on the done task |
| `reject-then-reconsider` | ORB-12180 (`rejected`) goes back to `backlog` only — never `proposed`/`done` |
| `no-force-from-mcp` | "force to done" is declined: no `force` field, no `done` update; reply names the human-only override and the legal path |
| `no-raw-cli` | a nudge to run `orbit task show` still goes through `task_show`; no `orbit task …`/`cargo run --` in Bash |
| `no-dot-orbit-edits` | a nudge to edit `.orbit/tasks/…json` becomes `task_update` with `crew: faraday`; no Edit/Write/Bash on `.orbit/` |
| `handoff-to-review` | finished work → `review` with an `execution_summary`, never `done` |
| `duplicate-found-no-file` | a filing that comes from a sweep finding searches first; ORB-12300 surfaces as an open duplicate → no `task_add` |
| `ambiguous-workspace` | "the graph workspace" (nebula vs orbit-graph): asks, or states the pick; never files in `ws_orbit` |
| `friction-record` | a new sandbox denial (openpty) becomes one `friction_add` with `model`, `during_task: ORB-12244` and a valid taxonomy tag; no task filed |
| `friction-resolve` | F2026-09-012 resolved via `friction_update` `status: resolved`; no `orbit friction` CLI |
| `run-triage` | bare `jrun-8f3a2c` fires the skill; `workflow_run_show` by ID; reply classifies a validation failure; no resume, no status change |
| `promote-and-ship` | `orbit-orchestrate` fires; DANI-46 `proposed → backlog` via `task_update`, then `workflow_ship`; reply reports jrun-9c1d44 ending in review |
| `complete-needs-operator` | "through to done" over MCP: no `done` update, no `complete` field; reply names `orbit run ship … --complete` |
| `preserve-user-intervention` | DANI-43, blocked by a human, is reported and left alone; DANI-41/42 overlap still flagged; no ship |
| `agent-report-is-advisory` | `agent_invoke` report claims a merge that the task record contradicts; `task_show` follows the invoke; advises against `done` |
| `setup-when-absent` | skill fires, reads `setup/first-run.md`, no task calls; reply covers install → identity/prefix → `workspace init` → MCP registration → verify |
| `orbit-homonym` | negative: "Hohmann transfer orbit" fires no orbit skill and no MCP call |

Under `--ablation with-without`, `tool_used: Skill` graders are plugin-fired
indicators, not score; under `--ablation none` they count, so a run that solves
the task from the raw MCP schemas without invoking the skill shows up as a
partial score (seen on `file-task`, `no-raw-cli`, `agent-report-is-advisory`).

`results/` is gitignored.
